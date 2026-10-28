"""TypeScript: every name in a type position is a use, not only the first identifier.

`a.T` references `T` (and reads the namespace `a`), `Array<Shape>` references `Shape`,
a union records every member, type arguments of calls, `new`, `as` and `satisfies` are
references, a type-alias body is walked, and a type parameter gives no edge.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

A_TS = """export interface Shape { a: number }
export type T = { a: number };
"""

B_LINES = [
    'import * as a from "./a";',
    'import { Shape } from "./a";',
    "export const v = 1;",
    "let t: a.T = { a: 1 };",
    "const g: Array<Shape> = [];",
    "let u: Shape | a.T | null = null;",
    "function f<X>(x: X): X { return x; }",
    "class Box<X> { item: X; constructor(item: X) { this.item = item; } }",
    "const s: Shape = { a: 1 };",
    "f<Shape>(s);",
    "new Box<Shape>(s);",
    "const k: Array<typeof v> = [];",
    "type K = { s: Shape; t: a.T };",
    'type L = Shape["a"];',
    "const c1 = s as a.T;",
    "const c2 = s satisfies Shape;",
    "export function gen<X extends Shape = Shape>(x: X): X { return x; }",
    "export interface J extends a.Shape {}",
    "const h = s as unknown as (y: Shape) => void;",
]
B_TS = "\n".join(B_LINES) + "\n"


def col(line: int, needle: str, offset: int = 0) -> int:
    """1-based column of `needle` (first occurrence) in 1-based `line` of b.ts."""
    return B_LINES[line - 1].index(needle) + 1 + offset


def analyse(tmp_path: Path) -> callix.Graph:
    (tmp_path / "a.ts").write_text(A_TS)
    (tmp_path / "b.ts").write_text(B_TS)
    return callix.TypeScriptAdapter(resolve=True).analyze(str(tmp_path))


def at(graph: callix.Graph, kind: RelationKind, line: int, column: int) -> list[tuple[str, str, object, str | None]]:
    """(source, target, relation, access) of every `kind` edge whose span starts at line:column."""
    out = []
    for r in graph.relations:
        if r.kind is not kind:
            continue
        span = r.metadata.get("span")
        if span is None or (span.start_line, span.start_col) != (line, column):
            continue
        out.append(
            (
                graph.nodes[r.source_id].qualified_name,
                graph.nodes[r.target_id].qualified_name,
                r,
                r.metadata.get("access"),
            )
        )
    return out


def any_edge_at(graph: callix.Graph, line: int, column: int) -> list[object]:
    return [
        r
        for r in graph.relations
        if r.metadata.get("span") is not None
        and (r.metadata["span"].start_line, r.metadata["span"].start_col) == (line, column)
    ]


def test_nested_and_generic_type_identifiers_are_referenced(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    found = at(graph, RelationKind.HAS_TYPE, 4, 10)
    assert [(s, t) for s, t, _, _ in found] == [("b.t", "a.T")]
    assert found[0][2].metadata["resolution"] == "definition"
    target = next(n for n in graph.nodes.values() if n.qualified_name == "a.T")
    assert target.kind is NodeKind.TYPE_ALIAS
    found = at(graph, RelationKind.HAS_TYPE, 5, 16)
    assert [(s, t) for s, t, _, _ in found] == [("b.g", "a.Shape")]
    # the namespace prefix is a value read, from the same source
    refs = at(graph, RelationKind.REFERENCES, 4, 8)
    assert [(s, t, a) for s, t, _, a in refs] == [("b.t", "a", "read")]


def test_union_members_are_types(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    shape = at(graph, RelationKind.HAS_TYPE, 6, col(6, "Shape"))
    nested = at(graph, RelationKind.HAS_TYPE, 6, col(6, "a.T", 2))
    assert [(s, t) for s, t, _, _ in shape] == [("b.u", "a.Shape")]
    assert [(s, t) for s, t, _, _ in nested] == [("b.u", "a.T")]


def test_call_and_new_type_arguments_are_references(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    call_arg = at(graph, RelationKind.REFERENCES, 10, col(10, "Shape"))
    new_arg = at(graph, RelationKind.REFERENCES, 11, col(11, "Shape"))
    assert [t for _, t, _, _ in call_arg] == ["a.Shape"]
    assert [t for _, t, _, _ in new_arg] == ["a.Shape"]
    call_edges = at(graph, RelationKind.CALLS, 10, col(10, "f"))
    assert [t for _, t, _, _ in call_edges] == ["b.f"]
    new_edges = at(graph, RelationKind.CALLS, 11, col(11, "Box"))
    assert len(new_edges) == 1 and new_edges[0][1].startswith("b.Box")


def test_typeof_inside_type_argument_is_a_value_read(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    refs = at(graph, RelationKind.REFERENCES, 12, col(12, "v>"))
    assert [(t, a) for _, t, _, a in refs] == [("b.v", "read")]
    for r in graph.relations:
        span = r.metadata.get("span")
        if r.kind is RelationKind.CALLS and span is not None:
            assert span.start_line != 12


def test_type_alias_body_is_walked(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    s = at(graph, RelationKind.HAS_TYPE, 13, col(13, "Shape"))
    nested = at(graph, RelationKind.HAS_TYPE, 13, col(13, "a.T", 2))
    indexed = at(graph, RelationKind.HAS_TYPE, 14, col(14, "Shape"))
    assert [(a, b) for a, b, _, _ in s] == [("b.K.s", "a.Shape")]
    assert [(a, b) for a, b, _, _ in nested] == [("b.K.t", "a.T")]
    assert [(a, b) for a, b, _, _ in indexed] == [("b.L", "a.Shape")]


def test_as_and_satisfies_types_are_references(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    cast = at(graph, RelationKind.REFERENCES, 15, col(15, "a.T", 2))
    sat = at(graph, RelationKind.REFERENCES, 16, col(16, "Shape"))
    assert [t for _, t, _, _ in cast] == ["a.T"]
    assert [t for _, t, _, _ in sat] == ["a.Shape"]


def test_type_parameter_constraint_and_default(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    line = B_LINES[16]
    first = line.index("Shape") + 1
    second = line.rindex("Shape") + 1
    assert first != second
    for column in (first, second):
        found = at(graph, RelationKind.HAS_TYPE, 17, column)
        assert [(s, t) for s, t, _, _ in found] == [("b.gen", "a.Shape")]
    # every uppercase X on the line is a type parameter: no edge may start there
    for index, ch in enumerate(line):
        if ch == "X":
            assert not any_edge_at(graph, 17, index + 1)
    for node in graph.nodes.values():
        if node.kind is NodeKind.EXTERNAL_SYMBOL:
            assert not node.qualified_name.startswith("annotation@b.ts:17:")


def test_interface_extends_nested_base(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    found = [
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.INHERITS_FROM
    ]
    assert ("b.J", "a.Shape") in found


def test_function_type_parameter_names_are_not_reads(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    assert not any_edge_at(graph, 19, col(19, "y:"))
    # the parameter's type is still a use
    assert any_edge_at(graph, 19, col(19, "Shape"))
