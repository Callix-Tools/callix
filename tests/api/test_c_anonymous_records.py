"""A struct or union named only by a typedef is a class of that name.

`typedef struct { int len; } buf_t;` declares CLASS `buf_t` (beside the TYPE_ALIAS
`buf_t`; the kind is part of the id), its fields are `buf_t::len`, and a member
read through a `buf_t *` binds to them. An anonymous member of a named record
still lends its fields to that record (C11).
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind


def analyse(tmp_path: Path, files: dict[str, str]) -> callix.Graph:
    for name, text in files.items():
        (tmp_path / name).write_text(text)
    return callix.CAdapter().analyze(str(tmp_path))


def nodes(graph: callix.Graph, kind: NodeKind, qualified_name: str) -> list:
    return [n for n in graph.nodes.values() if n.kind == kind and n.qualified_name == qualified_name]


def references(graph: callix.Graph, line: int, source: str) -> list:
    found = []
    for r in graph.relations:
        if r.kind != RelationKind.REFERENCES:
            continue
        span = r.metadata.get("span")
        if span is None or span.start_line != line:
            continue
        if graph.nodes[r.source_id].qualified_name != source:
            continue
        found.append((graph.nodes[r.target_id], r))
    return found


BUF_H = "typedef struct {\n    int len;\n    char *data;\n} buf_t;\n"
USE_C = '#include "buf.h"\nint get(buf_t *b) {\n    return b->len;\n}\n'


def test_typedef_anonymous_struct_fields(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"buf.h": BUF_H, "use.c": USE_C})
    [cls] = nodes(graph, NodeKind.CLASS, "buf_t")
    [alias] = nodes(graph, NodeKind.TYPE_ALIAS, "buf_t")
    assert cls.id != alias.id
    [attr] = nodes(graph, NodeKind.ATTRIBUTE, "buf_t::len")
    assert any(
        r.kind == RelationKind.DECLARES and r.source_id == cls.id and r.target_id == attr.id
        for r in graph.relations
    )
    # The class takes the typedef's extent, so it contains the alias identifier.
    assert cls.span == alias.span
    edges = references(graph, 3, "get")
    assert [(t.qualified_name, t.kind) for t, _ in edges] == [("buf_t::len", NodeKind.ATTRIBUTE)]
    span = edges[0][1].metadata["span"]
    assert (span.start_line, span.start_col, span.end_line, span.end_col) == (3, 15, 3, 18)
    assert edges[0][1].metadata["resolution"] == "name"


def test_typedef_anonymous_union_fields(tmp_path: Path) -> None:
    src = "typedef union { int i; float f; } U;\nint get(U *u) {\n    return u->i;\n}\n"
    graph = analyse(tmp_path, {"u.c": src})
    assert len(nodes(graph, NodeKind.CLASS, "U")) == 1
    assert len(nodes(graph, NodeKind.ATTRIBUTE, "U::i")) == 1
    edges = references(graph, 3, "get")
    assert [t.qualified_name for t, _ in edges] == ["U::i"]


def test_nested_anonymous_union_is_hoisted(tmp_path: Path) -> None:
    src = (
        "struct s { int k; union { int a; float f; }; };\n"
        "int via_ptr(struct s *p) {\n"
        "    return p->a;\n"
        "}\n"
        "int via_val(struct s v) {\n"
        "    return v.a;\n"
        "}\n"
    )
    graph = analyse(tmp_path, {"s.c": src})
    assert len(nodes(graph, NodeKind.ATTRIBUTE, "s::a")) == 1
    assert [n.qualified_name for n in graph.nodes.values() if n.kind == NodeKind.CLASS] == ["s"]
    assert [t.qualified_name for t, _ in references(graph, 3, "via_ptr")] == ["s::a"]
    assert [t.qualified_name for t, _ in references(graph, 6, "via_val")] == ["s::a"]


def test_typedef_with_tag_is_unchanged(tmp_path: Path) -> None:
    src = "typedef struct tag { int x; } tag_t;\nint get(tag_t *t) {\n    return t->x;\n}\n"
    graph = analyse(tmp_path, {"t.c": src})
    assert len(nodes(graph, NodeKind.CLASS, "tag")) == 1
    assert len(nodes(graph, NodeKind.ATTRIBUTE, "tag::x")) == 1
    assert len(nodes(graph, NodeKind.TYPE_ALIAS, "tag_t")) == 1
    assert nodes(graph, NodeKind.CLASS, "tag_t") == []
    assert [t.qualified_name for t, _ in references(graph, 3, "get")] == ["tag::x"]


def test_anonymous_record_doc_comes_from_typedef(tmp_path: Path) -> None:
    src = "/* A growable buffer. */\ntypedef struct { int len; } buf_t;\n"
    graph = analyse(tmp_path, {"b.h": src})
    [cls] = nodes(graph, NodeKind.CLASS, "buf_t")
    assert "growable buffer" in cls.metadata.get("doc", "")
