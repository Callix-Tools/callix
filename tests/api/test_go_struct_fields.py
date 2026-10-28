"""Every name of a multi-name field, and the fields of an anonymous struct type, are ATTRIBUTEs."""

from __future__ import annotations

import shutil
from pathlib import Path

import callix
import pytest
from callix import NodeKind, RelationKind

needs_go = pytest.mark.skipif(shutil.which("go") is None, reason="the Go toolchain is not installed")

P = "example.com/m"

SOURCE = """package m

type Base struct{ Label string }

func (b Base) Describe() string { return b.Label }

type Item struct{}

type pad struct {
\ttop, right int
}

type Outer struct {
\tX struct {
\t\tIdx  int
\t\tBase
\t\tIn   Item
\t}
\tL    []struct{ A, B string }
\tP    *struct{ Q int }
\tM    map[string]*struct{ V int }
\tC    chan struct{ W int }
\tG    [3]struct{ Z int }
\tDeep struct{ Mid struct{ Leaf int } }
\ta, b Item
}

func use() int {
\tp := pad{top: 1, right: 2}
\treturn p.top + p.right
}

func anon(o Outer) int {
\to.X.Idx = 3
\tn := len(o.L[0].A) + len(o.L[0].B)
\treturn o.X.Idx + n + o.M["k"].V + (<-o.C).W + o.P.Q + o.G[0].Z + o.Deep.Mid.Leaf
}
"""


def analyse(tmp_path: Path, resolve: bool) -> callix.Graph:
    (tmp_path / "go.mod").write_text("module example.com/m\n\ngo 1.22\n")
    (tmp_path / "main.go").write_text(SOURCE)
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


def pairs(graph: callix.Graph, kind: RelationKind) -> list:
    return [(s, t) for s, t, _ in edges(graph, kind)]


def test_every_name_of_a_field_is_an_attribute(tmp_path: Path) -> None:
    graph = analyse(tmp_path, resolve=False)
    top = node_by_qname(graph, f"{P}.pad.top")
    right = node_by_qname(graph, f"{P}.pad.right")
    assert top.kind is NodeKind.ATTRIBUTE and right.kind is NodeKind.ATTRIBUTE
    assert top.metadata["name_span"] != right.metadata["name_span"]
    declares = pairs(graph, RelationKind.DECLARES)
    assert (f"{P}.pad", f"{P}.pad.top") in declares
    assert (f"{P}.pad", f"{P}.pad.right") in declares


def test_multi_name_field_has_a_type_per_name(tmp_path: Path) -> None:
    graph = analyse(tmp_path, resolve=False)
    for name in ("a", "b"):
        assert node_by_qname(graph, f"{P}.Outer.{name}").kind is NodeKind.ATTRIBUTE
    # Resolution is off, so HAS_TYPE edges do not exist; the annotation occurrences are
    # what the resolver would turn into them. Pin the node side only.


def test_anonymous_struct_fields_are_attributes(tmp_path: Path) -> None:
    graph = analyse(tmp_path, resolve=False)
    declares = pairs(graph, RelationKind.DECLARES)
    expected = {
        "X.<anon>.Idx": "X",
        "X.<anon>.Base": "X",
        "X.<anon>.In": "X",
        "L.<anon>.A": "L",
        "L.<anon>.B": "L",
        "P.<anon>.Q": "P",
        "M.<anon>.V": "M",
        "C.<anon>.W": "C",
        "G.<anon>.Z": "G",
        "Deep.<anon>.Mid": "Deep",
        "Deep.Mid.<anon>.Leaf": None,
    }
    for suffix, owner in expected.items():
        if owner is None:
            continue
        node = node_by_qname(graph, f"{P}.Outer.{suffix}")
        assert node.kind is NodeKind.ATTRIBUTE
        assert (f"{P}.Outer.{owner}", f"{P}.Outer.{suffix}") in declares
    leaf = node_by_qname(graph, f"{P}.Outer.Deep.<anon>.Mid.<anon>.Leaf")
    assert (f"{P}.Outer.Deep.<anon>.Mid", leaf.qualified_name) in declares
    # No node for the anonymous struct itself.
    assert not [n for n in graph.nodes.values() if n.qualified_name.endswith(".<anon>")]
    assert len([n for n in graph.nodes.values() if n.kind is NodeKind.CLASS]) == 4


def test_qualified_names_are_deterministic(tmp_path: Path) -> None:
    first = analyse(tmp_path, resolve=False).to_json()
    second = analyse(tmp_path, resolve=False).to_json()
    assert first == second


def test_nested_embedded_field_is_not_inheritance(tmp_path: Path) -> None:
    graph = analyse(tmp_path, resolve=True)
    assert not [p for p in pairs(graph, RelationKind.INHERITS_FROM) if p[0] == f"{P}.Outer"]
    has_type = pairs(graph, RelationKind.HAS_TYPE)
    assert (f"{P}.Outer.X.<anon>.Base", f"{P}.Base") in has_type
    assert (f"{P}.Outer.X.<anon>.In", f"{P}.Item") in has_type
    # X itself does not annotate the types that belong to its inner fields.
    assert not [p for p in has_type if p[0] == f"{P}.Outer.X"]
    assert (f"{P}.Outer.a", f"{P}.Item") in has_type
    assert (f"{P}.Outer.b", f"{P}.Item") in has_type


@needs_go
def test_selectors_and_keys_bind_to_the_fields(tmp_path: Path) -> None:
    graph = analyse(tmp_path, resolve=True)
    refs = edges(graph, RelationKind.REFERENCES)
    targets = {t for _, t, r in refs if r.metadata["resolution"] == "definition"}
    for qname in (
        f"{P}.pad.top",
        f"{P}.pad.right",
        f"{P}.Outer.X.<anon>.Idx",
        f"{P}.Outer.L.<anon>.A",
        f"{P}.Outer.L.<anon>.B",
        f"{P}.Outer.P.<anon>.Q",
        f"{P}.Outer.M.<anon>.V",
        f"{P}.Outer.C.<anon>.W",
        f"{P}.Outer.G.<anon>.Z",
        f"{P}.Outer.Deep.<anon>.Mid.<anon>.Leaf",
    ):
        assert qname in targets, qname
    for _, t, r in refs:
        if t.endswith("<anon>.Idx") or t.endswith("pad.right"):
            assert "span" in r.metadata
    keyed = [r for s, t, r in refs if t == f"{P}.pad.right" and r.metadata.get("access") == "write"]
    assert keyed, "the key `right:` must reference pad.right"
