"""A prototype, a forward declaration and an include operand have a position.

A header declaration is folded into the definition's node, which is right; but
the name position of every other declaring site has to survive somewhere, or an
index of the code has nothing on a prototype line. It lives in
`metadata["declared_at"]` as `{"file", "span"}` entries, and an `#include`
path literal is the IMPORT's `name_span` and the span of its RESOLVES_TO.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind


def build(root: Path, files: dict[str, str], adapter=callix.CAdapter):
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return adapter().analyze(str(root))


def tup(span) -> tuple[int, int, int, int]:
    return (span.start_line, span.start_col, span.end_line, span.end_col)


def sites(node) -> list[tuple[str, tuple[int, int, int, int]]]:
    return [(d["file"], tup(d["span"])) for d in node.metadata.get("declared_at", [])]


def by_name(graph, qualified_name: str, kind: NodeKind):
    found = [n for n in graph.nodes.values() if n.kind is kind and n.qualified_name == qualified_name]
    assert len(found) == 1, f"{qualified_name}: {found}"
    return found[0]


def targets(graph, source, kind: RelationKind):
    return [r for r in graph.relations if r.source_id == source.id and r.kind is kind]


A_H = "#ifndef A_H\n#define A_H\nint add(int a, int b);\n#endif\n"
A_C = '#include "a.h"\n#include <stdio.h>\n\nint add(int a, int b) { return a + b; }\n'
B_C = '#include "a.h"\n\nint twice(int x) { return add(x, x); }\n'


def test_a_prototype_keeps_its_name_position(tmp_path: Path) -> None:
    graph = build(tmp_path, {"a.h": A_H, "a.c": A_C, "b.c": B_C})
    add = by_name(graph, "add", NodeKind.FUNCTION)
    assert add.file_path == "a.c"
    assert add.metadata["declared_in"] == ["a.h"]
    assert sites(add) == [("a.h", (3, 5, 3, 8))]
    assert callix.SpanIndex.from_graph(graph).at("a.h", 3, 5) == add.id
    assert callix.SpanIndex.from_graph(graph).at("a.c", 4, 5) == add.id
    twice = by_name(graph, "twice", NodeKind.FUNCTION)
    assert "declared_at" not in twice.metadata
    for node in graph.nodes.values():
        if node.kind is NodeKind.PARAMETER:
            assert "declared_at" not in node.metadata


def test_include_operands_are_positioned(tmp_path: Path) -> None:
    graph = build(tmp_path, {"a.h": A_H, "a.c": A_C, "b.c": B_C})
    header = [n for n in graph.nodes.values() if n.kind is NodeKind.FILE and n.file_path == "a.h"][0]
    imp = by_name(graph, "a.c::a.h", NodeKind.IMPORT)
    assert tup(imp.metadata["name_span"]) == (1, 10, 1, 15)
    resolves = targets(graph, imp, RelationKind.RESOLVES_TO)
    assert [(r.target_id, tup(r.metadata["span"])) for r in resolves] == [(header.id, (1, 10, 1, 15))]
    # The file-level IMPORTS summary stays unpositioned.
    file_a = [n for n in graph.nodes.values() if n.kind is NodeKind.FILE and n.file_path == "a.c"][0]
    imports = [r for r in graph.relations if r.source_id == file_a.id and r.target_id == header.id and r.kind is RelationKind.IMPORTS]
    assert imports and all(not r.metadata for r in imports)
    # A system header is an external symbol, positioned the same way.
    stdio = by_name(graph, "a.c::stdio.h", NodeKind.IMPORT)
    assert tup(stdio.metadata["name_span"]) == (2, 10, 2, 19)
    (edge,) = targets(graph, stdio, RelationKind.RESOLVES_TO)
    assert graph.nodes[edge.target_id].kind is NodeKind.EXTERNAL_SYMBOL
    assert tup(edge.metadata["span"]) == (2, 10, 2, 19)


def test_computed_and_repeated_includes(tmp_path: Path) -> None:
    graph = build(
        tmp_path,
        {
            "a.h": A_H,
            "x.c": '#ifdef A\n#include "a.h"\n#else\n#include "a.h"\n#endif\n#include HDR\n',
        },
    )
    imports = [n for n in graph.nodes.values() if n.kind is NodeKind.IMPORT and n.qualified_name == "x.c::a.h"]
    assert len(imports) == 1
    edges = targets(graph, imports[0], RelationKind.RESOLVES_TO)
    assert sorted(tup(r.metadata["span"]) for r in edges) == [(2, 10, 2, 15), (4, 10, 4, 15)]
    computed = by_name(graph, "x.c::HDR", NodeKind.IMPORT)
    assert "name_span" in computed.metadata
    (edge,) = targets(graph, computed, RelationKind.RESOLVES_TO)
    assert "span" not in edge.metadata


def test_a_prototype_in_the_same_file_is_listed_only_in_declared_at(tmp_path: Path) -> None:
    graph = build(tmp_path, {"x.c": "int f(void);\nint f(void) { return 0; }\n"})
    f = by_name(graph, "f", NodeKind.FUNCTION)
    assert sites(f) == [("x.c", (1, 5, 1, 6))]
    assert f.metadata["declared_in"] == []


def test_prototype_only_in_two_headers(tmp_path: Path) -> None:
    graph = build(
        tmp_path,
        {"h1.h": "int g(void);\n", "h2.h": "int g(void);\n", "m.c": "int m(void) { return 0; }\n"},
    )
    g = by_name(graph, "g", NodeKind.FUNCTION)
    assert g.file_path == "h1.h"
    assert sites(g) == [("h2.h", (1, 5, 1, 6))]


def test_cpp_forward_declaration_and_prototype_parameter_type(tmp_path: Path) -> None:
    graph = build(
        tmp_path,
        {
            "a.hpp": "class Slice;\nbool Get(Slice* in);\n",
            "a.cc": '#include "a.hpp"\nclass Slice {};\nbool Get(Slice* in) { return true; }\n',
        },
        adapter=callix.CppAdapter,
    )
    slice_ = by_name(graph, "Slice", NodeKind.CLASS)
    get = by_name(graph, "Get(Slice*)", NodeKind.FUNCTION)
    assert sites(slice_) == [("a.hpp", (1, 7, 1, 12))]
    assert sites(get) == [("a.hpp", (2, 6, 2, 9))]
    typed = [
        r
        for r in graph.relations
        if r.kind is RelationKind.HAS_TYPE
        and r.target_id == slice_.id
        and r.metadata.get("file") == "a.hpp"
        and tup(r.metadata["span"]) == (2, 10, 2, 15)
    ]
    assert typed, "the prototype's parameter type must have a HAS_TYPE edge written in a.hpp"


def test_cpp_in_class_declarations(tmp_path: Path) -> None:
    graph = build(
        tmp_path,
        {
            "db.hpp": (
                "struct Options;\nclass DB {\n public:\n  DB();\n  ~DB();\n"
                "  int Get(int k) const;\n  static int count;\n};\nextern int g_x;\n"
            ),
            "db.cc": (
                '#include "db.hpp"\nstruct Options {};\nDB::DB() {}\nDB::~DB() {}\n'
                "int DB::Get(int k) const { return k; }\nint DB::count = 0;\nint g_x = 1;\n"
            ),
        },
        adapter=callix.CppAdapter,
    )
    named = {}
    for node in graph.nodes.values():
        for file, _ in sites(node):
            named.setdefault(node.name, set()).add(file)
    for name in ("Options", "~DB", "Get", "count", "g_x"):
        assert "db.hpp" in named.get(name, set()), f"{name}: {sorted(named)}"
