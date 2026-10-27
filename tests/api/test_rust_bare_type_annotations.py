"""A bare type name in an annotation position records an annotation (RUST-01).

`arg: Foo`, `-> Foo`, a field `k: Foo`, `const C: Foo` and `-> Self` used to emit
nothing, because the type node was itself a childless `type_identifier`.
"""

from __future__ import annotations

import shutil
from collections import Counter
from pathlib import Path

import pytest

import callix
from callix import RelationKind, ResolvedRef

needs_rust = pytest.mark.skipif(
    shutil.which("rust-analyzer") is None or shutil.which("cargo") is None,
    reason="rust-analyzer and cargo are not installed",
)

LIB = """pub struct Foo;

pub struct Holder {
    pub field: Foo,
}

pub fn take(arg: Foo) {}

pub fn make() -> Foo {
    Foo
}

pub const C: Foo = Foo;

pub static S: Foo = Foo;

pub fn wrapped(r: &Foo, o: Option<Foo>) {}

impl Foo {
    pub fn me(&self) -> Self {
        Foo
    }
}
"""

BARE = [
    "rust01::take.arg",
    "rust01::make",
    "rust01::Holder.field",
    "rust01::C",
    "rust01::S",
]


def write_crate(root: Path) -> None:
    (root / "Cargo.toml").write_text(
        '[package]\nname = "rust01"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\n'
    )
    (root / "src").mkdir()
    (root / "src" / "lib.rs").write_text(LIB)


def position_of(needle: str, occurrence_line: str) -> tuple[int, int]:
    """1-based (line, col) of `needle` on the first line containing `occurrence_line`."""
    for number, text in enumerate(LIB.splitlines(), start=1):
        if occurrence_line in text:
            return number, text.index(needle, text.index(occurrence_line)) + 1
    raise AssertionError(occurrence_line)


class Recorder:
    """Records every (line, col) queried in lib.rs; answers with an external symbol."""

    def __init__(self) -> None:
        self.positions: list[tuple[int, int]] = []

    def prepare(self, root, files) -> None:
        pass

    def resolve_all(self, queries):
        for path, line, col in queries:
            if Path(path).name == "lib.rs":
                self.positions.append((line, col))
        return [ResolvedRef("spied.Foo", None, 1, 1, "struct", "third_party") for _ in queries]

    def status(self) -> str:
        return "ok"


def has_type_sources(graph) -> Counter:
    return Counter(
        graph.nodes[r.source_id].qualified_name
        for r in graph.relations
        if r.kind is RelationKind.HAS_TYPE
    )


def test_bare_annotations_are_queried(tmp_path):
    write_crate(tmp_path)
    recorder = Recorder()
    callix.RustAdapter(resolver=recorder).analyze(str(tmp_path))

    for line_hint in ("arg: Foo", "-> Foo {", "field: Foo", "const C: Foo", "static S: Foo"):
        position = position_of("Foo", line_hint) if "Foo" in line_hint.split(":")[-1] or "->" in line_hint else None
        assert position in recorder.positions, (line_hint, position, recorder.positions)


def test_bare_annotations_become_one_has_type_edge_each(tmp_path):
    write_crate(tmp_path)
    graph = callix.RustAdapter(resolver=Recorder()).analyze(str(tmp_path))
    counts = has_type_sources(graph)

    for name in BARE:
        assert counts[name] == 1, (name, dict(counts))
    # Each type name is one edge per site: `&Foo` is one, `Option<Foo>` is two.
    assert counts["rust01::wrapped.r"] == 1, dict(counts)
    assert counts["rust01::wrapped.o"] == 2, dict(counts)


@needs_rust
def test_bare_annotations_resolve_to_the_type(tmp_path):
    write_crate(tmp_path)
    graph = callix.RustAdapter().analyze(str(tmp_path))

    edges = {}
    for r in graph.relations:
        if r.kind is RelationKind.HAS_TYPE:
            edges.setdefault(graph.nodes[r.source_id].qualified_name, []).append(r)

    for name in BARE:
        assert name in edges, (name, sorted(edges))
        (edge,) = edges[name]
        assert graph.nodes[edge.target_id].qualified_name == "rust01::Foo"
        assert edge.metadata["resolution"] == "definition"
        assert "span" in edge.metadata
    # `-> Self` is recorded as one annotation. Its target is not asserted: the
    # rust-analyzer answer is the impl block's symbol, not `rust01::Foo`.
    assert len(edges["rust01::Foo.me"]) == 1, sorted(edges)
