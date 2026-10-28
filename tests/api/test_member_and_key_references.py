"""TypeScript: a member read, a member write and an object-literal key are uses.

The interface member is a node; `s.kind` and `{ kind: 1 }` typed by the interface
must point at it. The key of an untyped literal declares the property and is no edge.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

A_TS = """export interface Shape {
  kind: string;
  size: number;
}
export function make(): Shape {
  return { kind: "sq", size: 1 };
}
"""

B_TS = """import { Shape } from "./a";
export function f(s: Shape) {
  const k = s.kind;
  return k;
}
"""


def analyse(tmp_path: Path, files: dict[str, str]) -> callix.Graph:
    for name, text in files.items():
        (tmp_path / name).write_text(text)
    return callix.TypeScriptAdapter(resolve=True).analyze(str(tmp_path))


def edges(graph: callix.Graph, kind: RelationKind) -> list[tuple[str, str, tuple[int, int, int, int], object]]:
    out = []
    for r in graph.relations:
        if r.kind is not kind or "span" not in r.metadata:
            continue
        s = r.metadata["span"]
        out.append(
            (
                graph.nodes[r.source_id].qualified_name,
                graph.nodes[r.target_id].qualified_name,
                (s.start_line, s.start_col, s.end_line, s.end_col),
                r.metadata.get("access"),
            )
        )
    return out


def test_property_read_and_literal_key_reference_member(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.ts": A_TS, "b.ts": B_TS})
    refs = edges(graph, RelationKind.REFERENCES)
    assert ("b.f", "a.Shape.kind", (3, 15, 3, 19), "read") in refs
    assert ("a.make", "a.Shape.kind", (6, 12, 6, 16), "read") in refs
    assert any(s == "a.make" and t == "a.Shape.size" and span[0] == 6 for s, t, span, _ in refs)
    for r in graph.relations:
        if r.kind is RelationKind.REFERENCES and graph.nodes[r.target_id].qualified_name == "a.Shape.kind":
            assert r.metadata["resolution"] == "definition"


def test_member_write_references_attribute(tmp_path: Path) -> None:
    source = A_TS + "export function g(s: Shape) {\n  s.size = 3;\n}\n"
    graph = analyse(tmp_path, {"a.ts": source})
    refs = edges(graph, RelationKind.REFERENCES)
    assert any(s == "a.g" and t == "a.Shape.size" and a == "write" for s, t, _, a in refs)
    assert any(s == "a.g" and t == "a.g.s" for s, t, _, _ in refs)


def test_untyped_literal_key_is_no_edge(tmp_path: Path) -> None:
    source = "const o = { kind: 'x' };\nexport function g() { return { other: 1 }; }\n"
    graph = analyse(tmp_path, {"main.ts": source})
    key_cols = {(1, 13), (2, 28)}
    for r in graph.relations:
        span = r.metadata.get("span")
        if span is not None and (span.start_line, span.start_col) in key_cols:
            raise AssertionError(f"unexpected edge at a literal key: {r}")
    for node in graph.nodes.values():
        if node.kind is NodeKind.EXTERNAL_SYMBOL:
            assert not node.qualified_name.startswith(("read@main.ts:1:13", "read@main.ts:2:28"))


def test_typed_literal_method_key_references_interface_method(tmp_path: Path) -> None:
    source = "interface H { handle(): void }\nconst h: H = { handle() {} };\n"
    graph = analyse(tmp_path, {"main.ts": source})
    refs = edges(graph, RelationKind.REFERENCES)
    assert any(t == "main.H.handle" and span[0] == 2 and a == "read" for _, t, span, a in refs)
    assert any(n.qualified_name == "main.h.handle" for n in graph.nodes.values())


def test_callee_is_not_also_a_read(tmp_path: Path) -> None:
    source = (
        "interface S { area(): number }\n"
        "export function f(s: S) {\n"
        "  return s.area();\n"
        "}\n"
    )
    graph = analyse(tmp_path, {"main.ts": source})
    calls = [e for e in edges(graph, RelationKind.CALLS) if e[2][0] == 3]
    assert len(calls) == 1 and calls[0][1] == "main.S.area"
    area = calls[0][2]
    assert not [e for e in edges(graph, RelationKind.REFERENCES) if e[2] == area]


def test_stdlib_member_read_falls_back_to_external(tmp_path: Path) -> None:
    source = "export function f(xs: string[]) {\n  return xs.length;\n}\n"
    graph = analyse(tmp_path, {"main.ts": source})
    found = [
        r
        for r in graph.relations
        if r.kind is RelationKind.REFERENCES
        and r.metadata.get("span") is not None
        and r.metadata["span"].start_line == 2
        and r.metadata["span"].start_col == 13
    ]
    assert len(found) == 1
    assert graph.nodes[found[0].target_id].kind is NodeKind.EXTERNAL_SYMBOL
    assert found[0].metadata["resolution"] == "external"


def test_no_duplicate_member_edges(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.ts": A_TS, "b.ts": B_TS})
    refs = edges(graph, RelationKind.REFERENCES)
    assert len(refs) == len(set(refs))
