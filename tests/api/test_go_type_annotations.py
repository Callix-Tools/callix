"""Go annotations: every named type of a parameter, result or field is a HAS_TYPE."""

from __future__ import annotations

import shutil
from collections import Counter
from pathlib import Path

import callix
import pytest
from callix import RelationKind

needs_go = pytest.mark.skipif(shutil.which("go") is None, reason="the Go toolchain is not installed")

P = "example.com/r."

SOURCE = """package r

type Accounts map[string]string

type KeyIdentity struct{}

type Handler func()

type Stack[T any] struct{}

type Svc interface {
	Use() Handler
}

func Bare(a Accounts) Handler { return nil }

func Compound(p func(KeyIdentity) bool) {}

func Generic(s *Stack[KeyIdentity]) {}

type Box[T any] struct{ items []T }

func (b *Box[T]) Put(v T) {}

func Map[K comparable, V any](m map[K]V) {}

func V(xs ...KeyIdentity) {}

type E struct {
	KeyIdentity
	k KeyIdentity
}
"""


def analyse(tmp_path: Path) -> callix.Graph:
    (tmp_path / "go.mod").write_text("module example.com/r\n\ngo 1.21\n")
    (tmp_path / "a.go").write_text(SOURCE)
    return callix.GoAdapter().analyze(str(tmp_path))


def has_type(graph: callix.Graph):
    return [r for r in graph.relations if r.kind is RelationKind.HAS_TYPE]


def pairs(graph: callix.Graph) -> set[tuple[str, str]]:
    return {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in has_type(graph)
    }


@needs_go
def test_every_type_identifier_gets_has_type(tmp_path: Path) -> None:
    found = pairs(analyse(tmp_path))
    for src, dst in [
        ("Bare.a", "Accounts"),
        ("Bare", "Handler"),
        ("Compound.p", "KeyIdentity"),
        ("Generic.s", "Stack"),
        ("Generic.s", "KeyIdentity"),
        ("Svc.Use", "Handler"),
    ]:
        assert (P + src, P + dst) in found, (src, dst)


@needs_go
def test_no_has_type_to_predeclared_names(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    targets = {
        graph.nodes[r.target_id].qualified_name
        for r in has_type(graph)
        if graph.nodes[r.source_id].qualified_name == P + "Compound.p"
    }
    assert targets == {P + "KeyIdentity"}


@needs_go
def test_type_parameters_are_not_annotated(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    for r in has_type(graph):
        source = graph.nodes[r.source_id].qualified_name
        if source in {P + "Box.items", P + "Box.Put.v", P + "Map.m"}:
            assert not graph.nodes[r.target_id].qualified_name.startswith("annotation@"), source


@needs_go
def test_no_double_occurrences(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    triples = Counter(
        (r.source_id, r.target_id, str(r.metadata.get("span"))) for r in has_type(graph)
    )
    assert all(n == 1 for n in triples.values()), triples
    found = pairs(graph)
    assert (P + "V.xs", P + "KeyIdentity") in found
    assert (P + "E.k", P + "KeyIdentity") in found
    assert (P + "E", P + "KeyIdentity") not in found
    inherits = {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.INHERITS_FROM
    }
    assert (P + "E", P + "KeyIdentity") in inherits
