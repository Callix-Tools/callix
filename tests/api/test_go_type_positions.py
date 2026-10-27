"""Go type positions: receivers, type-spec right-hand sides, var/const types,
constraints and everything inside a function body are HAS_TYPE edges."""

from __future__ import annotations

import shutil
from collections import Counter
from pathlib import Path

import callix
import pytest
from callix import RelationKind

needs_go = pytest.mark.skipif(shutil.which("go") is None, reason="the Go toolchain is not installed")

P = "example.com/r."

SOURCE_A = """package r

type Item struct{ N int }

type Items []Item

type Handler func(*Item)

func (i *Items) Len() int { return len(*i) }

func Use(a *Item) {}

func Build() {
	x := Item{N: 1}
	h := func(p *Item) {}
	h(&x)
	var y []Item
	_ = y
	_ = &Items{x}
}
"""

SOURCE_B = """package r

type Stringer interface{ S() string }

type Kind int

type G[T Stringer] []T

func F[T Stringer]() {}

var Global Item

const K Kind = 1

type Box[T any] struct{ v T }

func (b *Box[T]) Put() {}

func Gen[T any](x T) T { return x }

func Build2(v any) {
	_ = make([]Item, 0)
	_ = make(Items, 0)
	_ = new(Item)
	_ = v.(Item)
	switch v.(type) {
	case *Item:
	}
	_ = Box[Item]{}
	var c interface{ M() Item }
	_ = c
}

func Excl(x Item) {
	_ = Gen[Item](x)
	type local struct{ A Item }
	var l local
	_ = l
}
"""


def analyse(tmp_path: Path) -> callix.Graph:
    (tmp_path / "go.mod").write_text("module example.com/r\n\ngo 1.22\n")
    (tmp_path / "a.go").write_text(SOURCE_A)
    (tmp_path / "b.go").write_text(SOURCE_B)
    return callix.GoAdapter().analyze(str(tmp_path))


def loc(src: str, marker: str, word: str, last: bool = False) -> tuple[int, int]:
    """1-based (line, col) of `word` on the first line containing `marker`."""
    for number, line in enumerate(src.split("\n"), start=1):
        if marker in line:
            index = line.rindex(word) if last else line.index(word)
            return number, index + 1
    raise AssertionError(f"no line contains {marker!r}")


def edges(graph: callix.Graph) -> set[tuple[str, str, int, int]]:
    out = set()
    for r in graph.relations:
        if r.kind is not RelationKind.HAS_TYPE:
            continue
        span = r.metadata.get("span")
        if span is None:
            continue
        out.add(
            (
                graph.nodes[r.source_id].qualified_name,
                graph.nodes[r.target_id].qualified_name,
                span.start_line,
                span.start_col,
            )
        )
    return out


def pairs(graph: callix.Graph) -> set[tuple[str, str]]:
    return {(s, d) for s, d, _, _ in edges(graph)}


@needs_go
def test_type_positions_have_has_type_edges(tmp_path: Path) -> None:
    found = edges(analyse(tmp_path))
    a = SOURCE_A
    item, items = P + "Item", P + "Items"
    expected = [
        (P + "Items", item, *loc(a, "type Items []Item", "Item", last=True)),
        (P + "Handler", item, *loc(a, "type Handler", "Item")),
        (P + "Items.Len", items, *loc(a, "func (i *Items)", "Items")),
        (P + "Use.a", item, *loc(a, "func Use", "Item")),
        (P + "Build", item, *loc(a, "x := Item{", "Item")),
        (P + "Build", item, *loc(a, "h := func", "Item")),
        (P + "Build", item, *loc(a, "var y []Item", "Item")),
        (P + "Build", items, *loc(a, "&Items{x}", "Items")),
    ]
    for e in expected:
        assert e in found, e


@needs_go
def test_one_edge_per_site(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    counts: Counter[tuple[str, str, int, int, int, int]] = Counter()
    for r in graph.relations:
        if r.kind is not RelationKind.HAS_TYPE:
            continue
        s = r.metadata["span"]
        counts[(r.source_id, r.target_id, s.start_line, s.start_col, s.end_line, s.end_col)] += 1
        assert not graph.nodes[r.target_id].qualified_name.startswith("annotation@")
    assert counts
    assert all(n == 1 for n in counts.values()), [k for k, n in counts.items() if n > 1]


@needs_go
def test_other_body_positions(tmp_path: Path) -> None:
    found = edges(analyse(tmp_path))
    b = SOURCE_B
    item, items, box = P + "Item", P + "Items", P + "Box"
    expected = [
        (P + "Build2", item, *loc(b, "make([]Item", "Item")),
        (P + "Build2", items, *loc(b, "make(Items", "Items")),
        (P + "Build2", item, *loc(b, "new(Item)", "Item")),
        (P + "Build2", item, *loc(b, "v.(Item)", "Item")),
        (P + "Build2", item, *loc(b, "case *Item:", "Item")),
        (P + "Build2", box, *loc(b, "Box[Item]{}", "Box")),
        (P + "Build2", item, *loc(b, "Box[Item]{}", "Item")),
        (P + "Build2", item, *loc(b, "var c interface", "Item")),
    ]
    for e in expected:
        assert e in found, e


@needs_go
def test_constraints_and_var_types(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    found = pairs(graph)
    assert (P + "G", P + "Stringer") in found
    assert (P + "F", P + "Stringer") in found
    assert (P + "Global", P + "Item") in found
    assert (P + "K", P + "Kind") in found
    assert not [d for _, d in found if d.endswith(".T")]


@needs_go
def test_exclusions(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    found = edges(graph)
    b = SOURCE_B
    # F[T](x) parses as a conversion over a generic type: the function Gen is not a type.
    assert not [e for e in found if e[1] == P + "Gen"]
    assert (P + "Excl", P + "Item", *loc(b, "Gen[Item](x)", "Item")) in found
    # A type declared inside a function is not a node: nothing targets it.
    assert not [e for e in found if "local" in e[1]]
    assert not [n for n in graph.nodes.values() if "local" in n.qualified_name and n.qualified_name.startswith("annotation@")]
    assert (P + "Excl", P + "Item", *loc(b, "type local struct", "Item")) in found
    # A generic receiver annotates the type, not its parameter.
    assert (P + "Box.Put", P + "Box", *loc(b, "func (b *Box[T]) Put", "Box")) in found
    assert not [e for e in found if e[1].endswith(".T")]
