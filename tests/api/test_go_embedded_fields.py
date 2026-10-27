"""An embedded struct field is an ATTRIBUTE named after its type."""

from __future__ import annotations

import shutil
from pathlib import Path

import callix
import pytest
from callix import NodeKind, RelationKind

needs_go = pytest.mark.skipif(shutil.which("go") is None, reason="the Go toolchain is not installed")

P = "example.com/m"

BASIC = """package main

type Group struct{}

func (g *Group) Use() {}

type Engine struct {
\tGroup
\tname string
}

func run(e *Engine) {
\te.Group.Use()
}
"""


def analyse(tmp_path: Path, source: str, resolve: bool = True, extra: dict | None = None) -> callix.Graph:
    (tmp_path / "go.mod").write_text("module example.com/m\n\ngo 1.22\n")
    (tmp_path / "main.go").write_text(source)
    for rel, text in (extra or {}).items():
        target = tmp_path / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
    return callix.GoAdapter(resolve=resolve).analyze(str(tmp_path))


def node_by_qname(graph: callix.Graph, qname: str):
    found = [n for n in graph.nodes.values() if n.qualified_name == qname]
    assert found, f"no node {qname}"
    return found[0]


def edges(graph: callix.Graph, kind: RelationKind) -> list:
    return [
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name, r)
        for r in graph.relations
        if r.kind is kind
    ]


def refs_to(graph: callix.Graph, target: str) -> list:
    return [(s, r) for s, t, r in edges(graph, RelationKind.REFERENCES) if t == target]


@needs_go
def test_embedded_field_is_attribute_and_selector_resolves(tmp_path: Path) -> None:
    graph = analyse(tmp_path, BASIC)
    attr = node_by_qname(graph, f"{P}.Engine.Group")
    assert attr.kind is NodeKind.ATTRIBUTE
    declares = [(s, t) for s, t, _ in edges(graph, RelationKind.DECLARES)]
    assert (f"{P}.Engine", f"{P}.Engine.Group") in declares
    inherits = [(s, t) for s, t, _ in edges(graph, RelationKind.INHERITS_FROM)]
    assert (f"{P}.Engine", f"{P}.Group") in inherits
    has_type = [(s, t) for s, t, _ in edges(graph, RelationKind.HAS_TYPE)]
    assert (f"{P}.Engine.Group", f"{P}.Group") in has_type
    found = [(s, r) for s, r in refs_to(graph, f"{P}.Engine.Group") if s == f"{P}.run"]
    assert found
    span = found[0][1].metadata["span"]
    assert (span.start_line, span.start_col) == (13, 4)
    assert found[0][1].metadata["access"] == "read"
    assert found[0][1].metadata["resolution"] != "unlocated"
    assert not [n for n in graph.nodes.values() if n.qualified_name == "read@main.go:13:4"]
    calls = [(s, t) for s, t, _ in edges(graph, RelationKind.CALLS)]
    assert (f"{P}.run", f"{P}.Group.Use") in calls


POINTER = """package main

type Group struct{}

type Engine struct {
\t*Group
}

func run(e *Engine) {
\t_ = e.Group
}
"""


@needs_go
def test_pointer_embedded_field(tmp_path: Path) -> None:
    graph = analyse(tmp_path, POINTER)
    attr = node_by_qname(graph, f"{P}.Engine.Group")
    assert attr.kind is NodeKind.ATTRIBUTE
    found = [r for s, r in refs_to(graph, f"{P}.Engine.Group") if s == f"{P}.run"]
    assert found
    assert found[0].metadata["resolution"] != "unlocated"


KEYED = """package main

type Group struct{}

type Engine struct {
\tGroup
}

func New() *Engine {
\treturn &Engine{Group: Group{}}
}
"""


@needs_go
def test_keyed_literal_key_of_embedded_field(tmp_path: Path) -> None:
    graph = analyse(tmp_path, KEYED)
    found = [r for s, r in refs_to(graph, f"{P}.Engine.Group") if s == f"{P}.New"]
    assert found
    assert found[0].metadata["access"] == "write"
    span = found[0].metadata["span"]
    assert (span.start_line, span.start_col) == (10, 17)


ARG = """package main

type Group struct{}

type Engine struct {
\tGroup
}

func take(g Group) {}

func run(e *Engine) {
\ttake(e.Group)
}
"""


@needs_go
def test_embedded_field_passed_as_argument(tmp_path: Path) -> None:
    graph = analyse(tmp_path, ARG)
    found = [r for s, r in refs_to(graph, f"{P}.Engine.Group") if s == f"{P}.run"]
    assert found


QUALIFIED = """package main

import "example.com/m/sub"

type Engine struct {
\tsub.Base
\tsub.Box[int]
}

func run(e *Engine) {
\t_ = e.Base
\t_ = e.Box
}
"""

SUB = """package sub

type Base struct{}

type Box[T any] struct{ v T }
"""


@needs_go
def test_qualified_and_generic_embedded_fields(tmp_path: Path) -> None:
    graph = analyse(tmp_path, QUALIFIED, extra={"sub/sub.go": SUB})
    for name in ("Base", "Box"):
        assert node_by_qname(graph, f"{P}.Engine.{name}").kind is NodeKind.ATTRIBUTE
        found = [r for s, r in refs_to(graph, f"{P}.Engine.{name}") if s == f"{P}.run"]
        assert found, name
        assert found[0].metadata["resolution"] != "unlocated"
    inherits = [(s, t) for s, t, _ in edges(graph, RelationKind.INHERITS_FROM)]
    assert (f"{P}.Engine", f"{P}/sub.Box") in inherits
    module_refs = [
        s for s, t, _ in edges(graph, RelationKind.REFERENCES) if t == f"{P}/sub"
    ]
    assert f"{P}.Engine.Base" not in module_refs
    assert f"{P}.Engine.Box" not in module_refs


def test_embedded_field_structure_without_resolver(tmp_path: Path) -> None:
    graph = analyse(tmp_path, BASIC, resolve=False)
    attr = node_by_qname(graph, f"{P}.Engine.Group")
    assert attr.kind is NodeKind.ATTRIBUTE
    declares = [(s, t) for s, t, _ in edges(graph, RelationKind.DECLARES)]
    assert (f"{P}.Engine", f"{P}.Engine.Group") in declares
    assert any(t == f"{P}.Engine.Group" for _, t in declares)
