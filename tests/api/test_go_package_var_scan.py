"""Go package-level var/const initializers are scanned with the variable as the enclosing node."""

from __future__ import annotations

import shutil
from pathlib import Path

import callix
import pytest
from callix import NodeKind, RelationKind

needs_go = pytest.mark.skipif(shutil.which("go") is None, reason="the Go toolchain is not installed")

P = "example.com/m"

MAIN = """package m

import "example.com/m/pkg"

func helper() int { return 1 }

func pair() (int, int) { return 1, 2 }

type T struct{}

func (T) Do() {}

var x = helper()

var fn = func(t T) {
	t.Do()
}

const A = pkg.B

var p, q = helper(), pkg.Make()

var a, b = pair()

var hs = []func(u T){func(u T) { u.Do() }}
"""

PKG = """package pkg

const B = 2

func Make() int { return B }
"""


def analyse(tmp_path: Path) -> callix.Graph:
    (tmp_path / "go.mod").write_text("module example.com/m\n\ngo 1.21\n")
    (tmp_path / "main.go").write_text(MAIN)
    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "pkg.go").write_text(PKG)
    return callix.GoAdapter().analyze(str(tmp_path))


def edges(graph: callix.Graph, kind: RelationKind) -> list:
    return [
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name, r)
        for r in graph.relations
        if r.kind is kind
    ]


@needs_go
def test_func_literal_parameter_is_a_node(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    params = [n for n in graph.nodes.values() if n.kind is NodeKind.PARAMETER]
    assert f"{P}.fn.t" in {n.qualified_name for n in params}
    declares = {(s, t) for s, t, _ in edges(graph, RelationKind.DECLARES)}
    assert (f"{P}.fn", f"{P}.fn.t") in declares


@needs_go
def test_calls_in_initializers(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    found = {(s, t): r for s, t, r in edges(graph, RelationKind.CALLS)}
    for key in [(f"{P}.x", f"{P}.helper"), (f"{P}.fn", f"{P}.T.Do")]:
        assert key in found, key
        assert found[key].metadata.get("span") is not None
        assert found[key].metadata.get("resolution") == "definition"


@needs_go
def test_const_selector_is_a_reference(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    refs = {(s, t): r for s, t, r in edges(graph, RelationKind.REFERENCES)}
    assert (f"{P}.A", f"{P}/pkg.B") in refs
    assert refs[(f"{P}.A", f"{P}/pkg.B")].metadata.get("span") is not None
    assert (f"{P}.A", f"{P}/pkg") in refs


@needs_go
def test_multi_value_specs_scan_each_site_once(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    calls = [(s, t) for s, t, _ in edges(graph, RelationKind.CALLS)]
    assert calls.count((f"{P}.p", f"{P}.helper")) == 1
    assert calls.count((f"{P}.q", f"{P}/pkg.Make")) == 1
    assert (f"{P}.p", f"{P}/pkg.Make") not in calls
    assert (f"{P}.q", f"{P}.helper") not in calls
    assert calls.count((f"{P}.a", f"{P}.pair")) == 1
    assert not [c for c in calls if c[0] == f"{P}.b"]


@needs_go
def test_nested_literal_is_a_closure(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    calls = [(s, t) for s, t, _ in edges(graph, RelationKind.CALLS)]
    assert (f"{P}.hs", f"{P}.T.Do") in calls
    assert not [n for n in graph.nodes.values() if n.kind is NodeKind.PARAMETER and n.name == "u"]
    has_type = [(s, t) for s, t, _ in edges(graph, RelationKind.HAS_TYPE)]
    assert (f"{P}.hs", f"{P}.T") in has_type
