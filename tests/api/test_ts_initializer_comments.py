"""A comment in front of an initializer must not hide what it initializes.

tree-sitter makes a comment a named extra node, so an initializer found as "the
first named child that is not the name" was the comment, and every call inside
the initializer was lost. Methods of an object literal handed to a call are
inline closures of the containing scope.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import RelationKind


def call_edges(tmp_path: Path, source: str) -> list:
    (tmp_path / "main.ts").write_text(source)
    graph = callix.TypeScriptAdapter().analyze(str(tmp_path))
    return [
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name, r)
        for r in graph.relations
        if r.kind is RelationKind.CALLS
    ]


def calls(tmp_path: Path, source: str) -> set[tuple[str, str]]:
    return {(s, t) for s, t, _ in call_edges(tmp_path, source)}


HEAD = "function f(x: number) { return x; }\nfunction g() { return 1; }\n"


def test_pure_annotation_before_the_initializer(tmp_path: Path) -> None:
    found = calls(tmp_path, HEAD + "const a = /*@__PURE__*/ f(g());\n")
    assert ("main.ts", "main.f") in found
    assert ("main.ts", "main.g") in found


def test_comment_between_annotation_and_initializer(tmp_path: Path) -> None:
    found = calls(tmp_path, HEAD + "const b: number = /* c */ f(g());\n")
    assert ("main.ts", "main.f") in found
    assert ("main.ts", "main.g") in found


def test_object_literal_methods_in_an_argument(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        "function run(o: object) { return o; }\n"
        "function target() { return 1; }\n"
        "function other() { return 2; }\n"
        "run({ m() { target(); }, get p() { return other(); } });\n",
    )
    assert ("main.ts", "main.target") in found
    assert ("main.ts", "main.other") in found


def test_class_field_with_a_leading_comment(tmp_path: Path) -> None:
    found = calls(tmp_path, HEAD + "class C {\n  x = /* c */ g();\n}\n")
    assert ("main.C", "main.g") in found


def test_pair_value_with_a_leading_comment(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        HEAD + "function run(o: object) { return o; }\nrun({ k: /* c */ g() });\n",
    )
    assert ("main.ts", "main.g") in found


def test_edges_from_a_commented_initializer_carry_a_span(tmp_path: Path) -> None:
    edges = call_edges(tmp_path, HEAD + "const a = /*@__PURE__*/ f(g());\n")
    assert edges
    assert all("span" in r.metadata for _, _, r in edges)
