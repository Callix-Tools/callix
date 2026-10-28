"""Type names in bodies, casts, sizeof, scopes, template arguments and signatures.

A type spelled in a body, and each class in a written scope, is a REFERENCES from
the enclosing function; the type of a declared node is its HAS_TYPE. Only a CLASS or
a TYPE_ALIAS is bound; a namespace or `sizeof(variable)` writes nothing.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

TYPES = (NodeKind.TYPE_ALIAS, NodeKind.CLASS)
# RelationKind is a PyO3 enum and is not iterable, so every kind is listed.
ALL_RELATIONS = tuple(
    getattr(RelationKind, name)
    for name in (
        "CALLS",
        "COMMUNICATES_WITH",
        "CONSUMES",
        "CONTAINS",
        "DECLARES",
        "DEPENDS_ON",
        "EXPOSES",
        "HAS_TYPE",
        "IMPORTS",
        "INHERITS_FROM",
        "REFERENCES",
        "RESOLVES_TO",
    )
)


def analyse(tmp_path: Path, files: dict[str, str], adapter: type) -> callix.Graph:
    for name, text in files.items():
        (tmp_path / name).write_text(text)
    return adapter().analyze(str(tmp_path))


def edges(graph: callix.Graph, kinds: tuple = (RelationKind.REFERENCES, RelationKind.HAS_TYPE)) -> list:
    """(source node, target node, relation, line, col) for every edge with a span."""
    found = []
    for r in graph.relations:
        if r.kind not in kinds:
            continue
        span = r.metadata.get("span")
        if span is None:
            continue
        found.append((graph.nodes[r.source_id], graph.nodes[r.target_id], r, span.start_line, span.start_col))
    return found


def at(graph: callix.Graph, line: int, col: int) -> list:
    return [e for e in edges(graph, ALL_RELATIONS) if (e[3], e[4]) == (line, col)]


C_H = "typedef struct S { int x; } S;\ntypedef struct T { int y; } T;\n"
C_C = (
    '#include "a.h"\n'
    "int f(void *p) {\n"
    "    S loc;\n"
    "    T *q = (T *)p;\n"
    "    int n = sizeof(S);\n"
    "    return loc.x + q->y + n;\n"
    "}\n"
)


def test_c_body_types(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.h": C_H, "a.c": C_C}, callix.CAdapter)
    want = {(3, 5): "S", (4, 5): "T", (4, 13): "T", (5, 20): "S"}
    got = {}
    for source, target, rel, line, col in edges(graph, (RelationKind.REFERENCES,)):
        if source.qualified_name == "f" and (line, col) in want:
            got[(line, col)] = (target, rel)
    assert set(got) == set(want)
    for pos, name in want.items():
        target, rel = got[pos]
        assert target.qualified_name == name
        assert target.kind in TYPES
        assert rel.metadata["resolution"] == "name"


CPP_HH = "struct Status {};\nclass Impl {\n public:\n  Status Get();\n};\n"
CPP_CC = (
    '#include "a.hh"\n'
    "Status Make();\n"
    "Status Impl::Get() {\n"
    "  Status s = Make();\n"
    "  const Status* p = static_cast<const Status*>(&s);\n"
    "  return *p;\n"
    "}\n"
)


def test_cpp_body_types_and_scope(tmp_path: Path) -> None:
    # The header is `.hh` so the test does not depend on how a `.h` is claimed.
    graph = analyse(tmp_path, {"a.hh": CPP_HH, "a.cc": CPP_CC}, callix.CppAdapter)
    want = {(3, 8): "Impl", (4, 3): "Status", (5, 9): "Status", (5, 39): "Status"}
    got = {}
    for source, target, rel, line, col in edges(graph, (RelationKind.REFERENCES,)):
        if source.qualified_name == "Impl::Get()" and (line, col) in want:
            got[(line, col)] = (target, rel)
    assert set(got) == set(want)
    for pos, name in want.items():
        target, rel = got[pos]
        assert target.qualified_name == name
        assert target.kind in TYPES
        assert rel.metadata.get("file", "a.cc") == "a.cc"


def test_function_pointer_typedef_and_variable(tmp_path: Path) -> None:
    header = (
        "typedef struct S { int x; } S;\n"
        "struct T { int y; };\n"
        "typedef S *(*handler)(struct T *t, S s);\n"
        "int (*cb)(S *s);\n"
        "struct ops { int (*open)(S *); };\n"
    )
    graph = analyse(tmp_path, {"a.h": header}, callix.CAdapter)
    has_type = {}
    for source, target, _rel, _line, _col in edges(graph, (RelationKind.HAS_TYPE,)):
        has_type.setdefault((source.kind, source.name), set()).add((target.qualified_name, target.kind))
    handler = has_type[(NodeKind.TYPE_ALIAS, "handler")]
    assert ("S", NodeKind.TYPE_ALIAS) in handler or ("S", NodeKind.CLASS) in handler
    assert ("T", NodeKind.CLASS) in handler  # `struct T` binds the struct
    assert any(name == "S" for name, _ in has_type[(NodeKind.VARIABLE, "cb")])
    assert any(name == "S" for name, _ in has_type[(NodeKind.ATTRIBUTE, "open")])


def test_library_type_is_not_a_project_type(tmp_path: Path) -> None:
    files = {
        "a.hpp": "namespace mine { class string {}; }\n",
        "b.cc": (
            '#include "a.hpp"\n'
            "#include <string>\n"
            "void g(void *p, std::string x) {\n"
            "  std::string a;\n"
            "  std::string b = x;\n"
            "  auto q = static_cast<std::string*>(p);\n"
            "}\n"
        ),
    }
    graph = analyse(tmp_path, files, callix.CppAdapter)
    for _source, target, _rel, _line, _col in edges(graph, ALL_RELATIONS):
        assert target.qualified_name != "mine::string"
    for node in graph.nodes.values():
        if node.kind is NodeKind.EXTERNAL_SYMBOL:
            assert not node.qualified_name.startswith("std@")
    # `std::string a;` is a construction of a library class: nothing at the type.
    assert not [e for e in at(graph, 4, 8) if e[2].kind in (RelationKind.REFERENCES, RelationKind.HAS_TYPE)]


def test_construction_is_not_doubled(tmp_path: Path) -> None:
    source = "class Engine { public: Engine(); };\nvoid g() { Engine e; }\n"
    graph = analyse(tmp_path, {"a.cc": source}, callix.CppAdapter)
    found = at(graph, 2, 12)
    assert [e[2].kind for e in found] == [RelationKind.CALLS]


def test_sizeof_variable_and_template_parameter(tmp_path: Path) -> None:
    source = (
        "int buf[4];\n"
        "template <class T> void g() { T x; (void)static_cast<T>(0); int n = sizeof(buf); }\n"
    )
    graph = analyse(tmp_path, {"a.cc": source}, callix.CppAdapter)
    for node in graph.nodes.values():
        if node.kind is NodeKind.EXTERNAL_SYMBOL:
            assert not node.qualified_name.startswith(("T@", "buf@"))


def test_namespace_qualifier_writes_nothing(tmp_path: Path) -> None:
    source = "namespace ns { int f(); }\nvoid g() { ns::f(); }\n"
    graph = analyse(tmp_path, {"a.cc": source}, callix.CppAdapter)
    assert not [e for e in at(graph, 2, 12) if e[2].kind is RelationKind.REFERENCES]
