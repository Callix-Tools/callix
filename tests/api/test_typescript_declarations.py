"""TypeScript: type-literal members, namespaces and string-named members are nodes.

A member of an object type alias is `Alias.member`; a `namespace` is a MODULE whose
members are `N.member`; ambient (`declare`) function signatures declare the function.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

A_TS = """export type Res = { success: true; data: number };
export namespace NS {
  export function nf() {}
}
export interface I { "~std": number }
"""

B_TS = """import { Res, NS } from "./a";
export function f(r: Res) {
  NS.nf();
  return r.success;
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


def declares(graph: callix.Graph, parent: str, child: str) -> bool:
    names = by_name(graph)
    return any(
        r.kind is RelationKind.DECLARES
        and r.source_id == names[parent].id
        and r.target_id == names[child].id
        for r in graph.relations
    )


def test_type_literal_and_namespace_members(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.ts": A_TS, "b.ts": B_TS})
    names = by_name(graph)
    assert names["a.NS"].kind is NodeKind.MODULE
    assert names["a.NS"].metadata["is_namespace"] is True
    assert names["a.NS.nf"].kind is NodeKind.FUNCTION
    assert names["a.Res.success"].kind is NodeKind.ATTRIBUTE
    assert names["a.Res.data"].kind is NodeKind.ATTRIBUTE
    assert names["a.I.~std"].kind is NodeKind.ATTRIBUTE
    assert declares(graph, "a.ts", "a.NS")
    assert declares(graph, "a.NS", "a.NS.nf")
    assert declares(graph, "a.Res", "a.Res.success")
    assert declares(graph, "a.Res", "a.Res.data")
    assert declares(graph, "a.I", "a.I.~std")


def test_type_literal_and_namespace_uses_resolve(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.ts": A_TS, "b.ts": B_TS}, resolve=True)
    found = {}
    for r in graph.relations:
        if r.kind not in (RelationKind.CALLS, RelationKind.REFERENCES):
            continue
        source = graph.nodes[r.source_id].qualified_name
        target = graph.nodes[r.target_id].qualified_name
        found[(r.kind, source, target)] = r
    call = found[(RelationKind.CALLS, "b.f", "a.NS.nf")]
    assert call.metadata["resolution"] == "definition"
    read = found[(RelationKind.REFERENCES, "b.f", "a.Res.success")]
    assert read.metadata["resolution"] == "definition"
    assert read.metadata["access"] == "read"
    assert (RelationKind.REFERENCES, "b.f", "a.NS") in found


def test_dotted_namespace(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"main.ts": "namespace A.B { export function g() {} }\n"})
    names = by_name(graph)
    assert names["main.A"].kind is NodeKind.MODULE
    assert names["main.A.B"].kind is NodeKind.MODULE
    assert declares(graph, "main.A", "main.A.B")
    assert declares(graph, "main.A.B", "main.A.B.g")


def test_declare_namespace_signatures(tmp_path: Path) -> None:
    source = (
        "export declare namespace D {\n"
        "  export function h(x: number): void;\n"
        "  export function h(x: string): void;\n"
        "}\n"
    )
    graph = analyse(tmp_path, {"main.ts": source})
    funcs = [n for n in graph.nodes.values() if n.qualified_name == "main.D.h"]
    assert len(funcs) == 1
    assert funcs[0].kind is NodeKind.FUNCTION
    assert len(funcs[0].metadata["declared_at"]) == 2


def test_union_arm_members(tmp_path: Path) -> None:
    source = "type R = { ok: true; v: number } | { ok: false; e: string };\n"
    graph = analyse(tmp_path, {"main.ts": source})
    names = by_name(graph)
    assert [n for n in graph.nodes.values() if n.qualified_name == "main.R.ok"].__len__() == 1
    assert len(names["main.R.ok"].metadata["declared_at"]) == 1
    assert "main.R.v" in names
    assert "main.R.e" in names


def test_number_and_string_member_names(tmp_path: Path) -> None:
    source = 'interface J { 0: string; "a-b"(): void }\n'
    graph = analyse(tmp_path, {"main.ts": source})
    names = by_name(graph)
    assert names["main.J.0"].kind is NodeKind.ATTRIBUTE
    assert names["main.J.a-b"].kind is NodeKind.METHOD


def test_namespace_does_not_collide_with_file_module(tmp_path: Path) -> None:
    graph = analyse(
        tmp_path,
        {
            "a.ts": "export namespace sub { export const x = 1; }\n",
            "a/sub.ts": "export const y = 2;\n",
        },
    )
    modules = [n for n in graph.nodes.values() if n.qualified_name == "a.sub"]
    assert len(modules) == 1
    assert modules[0].kind is NodeKind.MODULE
    assert modules[0].file_path == "a/sub.ts"
    assert "a.sub.x" in by_name(graph)
