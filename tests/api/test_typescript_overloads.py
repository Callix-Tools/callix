"""TypeScript: overload signatures are `declared_at` spans of their implementation.

Exported, default-exported and namespace overloads, and file-level `declare function`.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

A_TS = """export function ov(x: number): void;
export function ov(x: string): void;
export function ov(x: any): void {}
export default function dv(x: number): void;
export default function dv(x: string): void;
export default function dv(x: any): void {}
"""

B_TS = """import dv from "./a";
import { ov } from "./a";
export function g() {
  ov(1);
  ov("s");
  dv(1);
  dv("s");
}
"""

MAIN_TS = """declare function amb(x: number): void;
declare function amb(x: string): void;
export declare function eamb(x: number): void;
export declare function eamb(x: string): void;
export function f() {
  amb(1);
  amb("s");
  eamb(1);
}
"""


def analyse(tmp_path: Path, files: dict[str, str], resolve: bool = False) -> callix.Graph:
    for name, text in files.items():
        target = tmp_path / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
    return callix.TypeScriptAdapter(resolve=resolve).analyze(str(tmp_path))


def by_name(graph: callix.Graph) -> dict[str, callix.Node]:
    return {n.qualified_name: n for n in graph.nodes.values()}


def calls(graph: callix.Graph, source: str, target: str) -> list[callix.Relation]:
    names = by_name(graph)
    return [
        r
        for r in graph.relations
        if r.kind is RelationKind.CALLS
        and r.source_id == names[source].id
        and r.target_id == names[target].id
    ]


def test_exported_overload_declared_at(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.ts": A_TS})
    funcs = [n for n in graph.nodes.values() if n.qualified_name == "a.ov"]
    assert len(funcs) == 1
    assert funcs[0].kind is NodeKind.FUNCTION
    assert len(funcs[0].metadata["declared_at"]) >= 2


def test_exported_overload_calls_bind(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.ts": A_TS, "b.ts": B_TS}, resolve=True)
    edges = calls(graph, "b.g", "a.ov")
    assert len(edges) == 2
    for edge in edges:
        assert edge.metadata["resolution"] == "definition"
        assert "span" in edge.metadata


def test_default_export_overload_declared_at(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.ts": A_TS, "b.ts": B_TS}, resolve=True)
    funcs = [n for n in graph.nodes.values() if n.qualified_name == "a.dv"]
    assert len(funcs) == 1
    assert len(funcs[0].metadata["declared_at"]) >= 2
    edges = calls(graph, "b.g", "a.dv")
    assert len(edges) == 2
    assert all(e.metadata["resolution"] == "definition" for e in edges)


def test_declare_function_is_a_function(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"main.ts": MAIN_TS}, resolve=True)
    names = by_name(graph)
    for qname in ("main.amb", "main.eamb"):
        assert [n.qualified_name for n in graph.nodes.values()].count(qname) == 1
        assert names[qname].kind is NodeKind.FUNCTION
        assert len(names[qname].metadata["declared_at"]) == 2
        assert any(
            r.kind is RelationKind.DECLARES
            and r.source_id == names["main.ts"].id
            and r.target_id == names[qname].id
            for r in graph.relations
        )
    assert len(calls(graph, "main.f", "main.amb")) == 2
    assert len(calls(graph, "main.f", "main.eamb")) == 1
    for edge in calls(graph, "main.f", "main.amb") + calls(graph, "main.f", "main.eamb"):
        assert edge.metadata["resolution"] == "definition"


def test_declare_global_signature_gives_no_node(tmp_path: Path) -> None:
    source = (
        "export {};\n"
        "declare global {\n"
        "  function gg(x: number): void;\n"
        "  function gg(x: string): void;\n"
        "}\n"
    )
    graph = analyse(tmp_path, {"main.ts": source})
    assert not [n for n in graph.nodes.values() if n.qualified_name.endswith("gg")]
