"""Member reads, writes and initialisers are bound by the receiver's declared type.

A field access carries its receiver (a typed local, `this`, a field chain, a
typedef'd pointer, a file-scope variable); a known type without the field is an
external symbol; an unqualified name never binds to a field.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind


def analyse(tmp_path: Path, files: dict[str, str], adapter: type) -> callix.Graph:
    for name, text in files.items():
        (tmp_path / name).write_text(text)
    return adapter().analyze(str(tmp_path))


def on_line(graph: callix.Graph, kind: RelationKind, line: int, source: str | None = None) -> list:
    """(source node, target node, relation) of every edge of `kind` whose span starts on `line`."""
    found = []
    for r in graph.relations:
        if r.kind != kind:
            continue
        span = r.metadata.get("span")
        assert span is not None, "every resolved edge carries its span"
        if span.start_line != line:
            continue
        src = graph.nodes[r.source_id]
        if source is not None and src.qualified_name != source:
            continue
        found.append((src, graph.nodes[r.target_id], r))
    return found


C_REPRO = (
    "struct cmdArg { int type; };\n"
    "struct moduleValue { void *type; };\n"
    "struct srv { long mstime; };\n"
    "long mstime(void) { return 0; }\n"
    "void *get(struct moduleValue *mv) {\n"
    "    return mv->type;\n"
    "}\n"
    "long now(void) {\n"
    "    return mstime();\n"
    "}\n"
)


def test_c_read_is_bound_by_the_receiver_type(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.c": C_REPRO}, callix.CAdapter)
    edges = on_line(graph, RelationKind.REFERENCES, 6, "get")
    assert [(t.qualified_name, t.kind) for _, t, _ in edges] == [("moduleValue::type", NodeKind.ATTRIBUTE)]
    assert edges[0][2].metadata["resolution"] == "name"
    assert not edges[0][2].metadata.get("ambiguous")


def test_c_unqualified_call_is_not_a_field(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.c": C_REPRO}, callix.CAdapter)
    edges = on_line(graph, RelationKind.CALLS, 9, "now")
    assert [(t.qualified_name, t.kind) for _, t, _ in edges] == [("mstime", NodeKind.FUNCTION)]


CPP_H = (
    "struct Edit {\n"
    "  int next_;\n"
    "  int vset_;\n"
    "};\n"
    "struct Set {\n"
    "  Set() : next_(2) {}\n"
    "  int next_;\n"
    "  int vset_;\n"
    "};\n"
)
CPP_CC = '#include "a.hh"\nint f(Set* s) {\n  return s->vset_;\n}\n'


def test_cpp_read_and_initialiser_bind_to_the_right_class(tmp_path: Path) -> None:
    # The header is `.hh` so the test does not depend on how a `.h` is claimed.
    graph = analyse(tmp_path, {"a.hh": CPP_H, "a.cc": CPP_CC}, callix.CppAdapter)
    read = on_line(graph, RelationKind.REFERENCES, 3, "f(Set*)") or on_line(graph, RelationKind.REFERENCES, 3)
    assert [t.qualified_name for _, t, _ in read] == ["Set::vset_"]
    init = on_line(graph, RelationKind.REFERENCES, 6)
    assert [t.qualified_name for _, t, _ in init] == ["Set::next_"]
    assert init[0][2].metadata["resolution"] == "name"


def targets(graph: callix.Graph, line: int) -> list[str]:
    return sorted(t.qualified_name for _, t, _ in on_line(graph, RelationKind.REFERENCES, line))


def test_c_field_chain(tmp_path: Path) -> None:
    src = (
        "struct other { int len; int next; };\n"
        "struct node { struct node *next; int len; };\n"
        "int total(struct node *n) {\n"
        "    return n->next->len;\n"
        "}\n"
    )
    graph = analyse(tmp_path, {"a.c": src}, callix.CAdapter)
    assert targets(graph, 4) == ["node::len", "node::next"]


def test_c_typedef_pointer_receiver(tmp_path: Path) -> None:
    src = (
        "struct other { int hz; };\n"
        "struct srv { int hz; };\n"
        "typedef struct srv *srv_t;\n"
        "int f(srv_t s) {\n"
        "    return s->hz;\n"
        "}\n"
    )
    graph = analyse(tmp_path, {"a.c": src}, callix.CAdapter)
    assert targets(graph, 5) == ["srv::hz"]


def test_c_global_receiver(tmp_path: Path) -> None:
    src = (
        "struct other { int hz; };\n"
        "struct srv { int hz; };\n"
        "struct srv server;\n"
        "int f(void) {\n"
        "    return server.hz;\n"
        "}\n"
    )
    graph = analyse(tmp_path, {"a.c": src}, callix.CAdapter)
    assert targets(graph, 5) == ["srv::hz"]


def test_c_library_typed_receiver_is_external(tmp_path: Path) -> None:
    src = (
        "struct srv { int _flags; };\n"
        "int f(FILE *fp) {\n"
        "    return fp->_flags;\n"
        "}\n"
    )
    graph = analyse(tmp_path, {"a.c": src}, callix.CAdapter)
    edges = on_line(graph, RelationKind.REFERENCES, 3, "f")
    assert len(edges) == 1
    _, target, rel = edges[0]
    assert target.kind == NodeKind.EXTERNAL_SYMBOL
    assert rel.metadata["resolution"] == "external"


def test_cpp_annotation_never_lands_on_a_method(tmp_path: Path) -> None:
    src = (
        "struct Item {};\n"
        "struct Store {\n"
        "  int Item() { return 0; }\n"
        "};\n"
        "int f(struct Item* p) { return 0; }\n"
    )
    graph = analyse(tmp_path, {"a.cc": src}, callix.CppAdapter)
    for r in graph.relations:
        if r.kind == RelationKind.HAS_TYPE:
            assert graph.nodes[r.target_id].kind != NodeKind.METHOD
