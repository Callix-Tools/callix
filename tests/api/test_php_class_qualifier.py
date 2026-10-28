"""The left side of `::` is a class position: it never binds to a function.

Regression of Callix-Tools/callix#30: `Str::random()` with `use ...\\Str;` bound its
qualifier to the global `function str()` (PHP function names are case-insensitive).
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

STR_PHP = """<?php
namespace Illuminate\\Support;

class Str {
    const X = 1;
    public static $p = 2;
    public static function random(): string { return "x"; }
}

class Validator {}
"""

HELPERS_PHP = """<?php

if (! function_exists('str')) {
    function str($string = null) { return $string; }
}
if (! function_exists('validator')) {
    function validator() { return null; }
}
if (! function_exists('request')) {
    function request() { return null; }
}
"""

T_PHP = """<?php
namespace Tests;

use Illuminate\\Support\\Str;
use Illuminate\\Support\\Validator;

class T {
    public function run(): string {
        return Str::random();
    }

    public function others($o): array {
        $a = Validator::class;
        $b = new Str();
        $c = Str::X;
        $d = Str::$p;
        $e = $o instanceof Str;
        return Missing::go();
    }

    public function typed(Str $a): void {}
}
"""

STR = "Illuminate\\Support\\Str"
VALIDATOR = "Illuminate\\Support\\Validator"


def build(tmp_path: Path) -> callix.Graph:
    for rel, text in {
        "src/Support/Str.php": STR_PHP,
        "src/Support/helpers.php": HELPERS_PHP,
        "tests/T.php": T_PHP,
    }.items():
        path = tmp_path / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return callix.PhpAdapter().analyze(str(tmp_path))


def from_t(graph: callix.Graph):
    """CALLS / REFERENCES / HAS_TYPE edges whose source lives under Tests\\T."""
    kinds = (RelationKind.CALLS, RelationKind.REFERENCES, RelationKind.HAS_TYPE)
    return [
        r
        for r in graph.relations
        if r.kind in kinds and graph.nodes[r.source_id].qualified_name.startswith("Tests\\T")
    ]


def position(line_text: str, needle: str) -> tuple[int, int]:
    """1-based (line, column) of the first `needle` on the line holding `line_text`."""
    lines = T_PHP.splitlines()
    index = next(i for i, line in enumerate(lines) if line_text in line)
    return index + 1, lines[index].index(needle) + 1


def targets_at(graph: callix.Graph, line: int, col: int, kind: RelationKind):
    out = []
    for r in from_t(graph):
        span = r.metadata.get("span")
        if r.kind is kind and span is not None and (span.start_line, span.start_col) == (line, col):
            out.append(r)
    return out


def test_qualifier_binds_to_the_class_not_the_function(tmp_path: Path) -> None:
    graph = build(tmp_path)
    line, col = position("return Str::random()", "Str")
    hits = targets_at(graph, line, col, RelationKind.REFERENCES)
    names = [graph.nodes[r.target_id].qualified_name for r in hits]
    assert names == [STR], f"BUG: class qualifier Str binds to {names}"
    assert hits[0].metadata["resolution"] == "definition"
    calls = {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.CALLS
    }
    assert ("Tests\\T\\run", STR + "\\random") in calls


def test_no_edge_from_the_class_reaches_a_function(tmp_path: Path) -> None:
    graph = build(tmp_path)
    bad = [
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in from_t(graph)
        if graph.nodes[r.target_id].kind is NodeKind.FUNCTION
    ]
    assert bad == []


def test_class_constant_class_keyword(tmp_path: Path) -> None:
    graph = build(tmp_path)
    line, col = position("Validator::class", "Validator")
    hits = targets_at(graph, line, col, RelationKind.REFERENCES)
    assert [graph.nodes[r.target_id].qualified_name for r in hits] == [VALIDATOR]


def test_new_constant_property_and_instanceof_bind_to_the_class(tmp_path: Path) -> None:
    graph = build(tmp_path)
    for needle_line in ("new Str()", "Str::X", "Str::$p", "instanceof Str"):
        line, col = position(needle_line, "Str")
        hits = [
            r
            for r in from_t(graph)
            if r.metadata.get("span") is not None
            and (r.metadata["span"].start_line, r.metadata["span"].start_col) == (line, col)
            and graph.nodes[r.target_id].qualified_name == STR
        ]
        assert hits, f"no edge to the class Str for `{needle_line}`"


def test_parameter_type_is_the_class(tmp_path: Path) -> None:
    graph = build(tmp_path)
    assert ("Tests\\T\\typed\\a", STR) in {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.HAS_TYPE
    }


def test_undeclared_qualifier_is_namespaced_like_its_call(tmp_path: Path) -> None:
    graph = build(tmp_path)
    externals = {
        n.qualified_name for n in graph.nodes.values() if n.kind is NodeKind.EXTERNAL_SYMBOL
    }
    assert "Tests\\Missing" in externals
    assert "Missing" not in externals
