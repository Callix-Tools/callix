"""Every type name in a type position is an annotation (RUST-02).

Only the leftmost identifier of a type used to be recorded, and impl headers,
bounds, where clauses, `let`/alias/cast/closure types and tuple fields were not
visited at all.
"""

from __future__ import annotations

import shutil
from collections import Counter
from pathlib import Path

import pytest

import callix
from callix import NodeKind, RelationKind, ResolvedRef

needs_rust = pytest.mark.skipif(
    shutil.which("rust-analyzer") is None or shutil.which("cargo") is None,
    reason="rust-analyzer and cargo are not installed",
)

REPRO = """pub struct Matcher;
pub struct Error;
pub struct Builder;

pub fn build() -> Result<Matcher, Error> {
    Err(Error)
}

impl Builder {
    pub fn new() -> Builder {
        Builder
    }
}

pub type Alias = Matcher;

pub trait Tr {}

impl Tr for Vec<Matcher> {}
"""

POSITIONS = """pub struct Foo;
pub struct Bar;
pub trait Bound {}
pub struct Def;
pub type Alias = Foo;
pub struct P(Foo, pub Bar);
pub enum E {
    A(Foo),
    B { b: Bar },
}
pub struct W<T: Bound = Def>(T);
pub fn f<U: Bound>(u: U) where Vec<U>: Bound {}
pub fn body() {
    let x: Foo;
    let c = 1 as Foo;
    let k = |a: Foo| -> Bar { loop {} };
}
impl From<Foo> for Bar {}
"""

HEADER = """pub struct Foo;
pub struct Bar;
pub trait Tr {}
impl Tr for Ext {}
impl From<Foo> for Bar {}
"""


def write_crate(root: Path, name: str, lib: str) -> None:
    (root / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\n'
    )
    (root / "src").mkdir()
    (root / "src" / "lib.rs").write_text(lib)


def pos(src: str, hint: str, ident: str) -> tuple[int, int]:
    """1-based (line, col) of `ident` after `hint` on the first line holding `hint`."""
    for number, text in enumerate(src.splitlines(), start=1):
        if hint in text:
            return number, text.index(ident, text.index(hint)) + 1
    raise AssertionError(hint)


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


@needs_rust
def test_every_type_identifier_has_an_edge(tmp_path):
    write_crate(tmp_path, "r02", REPRO)
    graph = callix.RustAdapter().analyze(str(tmp_path))

    spans = {}
    for r in graph.relations:
        if r.kind in (
            RelationKind.HAS_TYPE,
            RelationKind.REFERENCES,
            RelationKind.INHERITS_FROM,
            RelationKind.CALLS,
        ):
            span = r.metadata.get("span")
            if span is not None:
                spans[(graph.nodes[r.target_id].qualified_name, span.start_line, span.start_col)] = r

    for want in [
        ("r02::Matcher", 5, 26),
        ("r02::Error", 5, 35),
        ("r02::Builder", 9, 6),
        ("r02::Matcher", 15, 18),
    ]:
        assert want in spans, (want, sorted(spans))
        assert spans[want].metadata["resolution"] == "definition"

    # `impl Tr for Vec<Matcher>`: the self type is not a project class, the trait
    # still becomes an INHERITS_FROM, from the file.
    inherits = [
        r
        for r in graph.relations
        if r.kind is RelationKind.INHERITS_FROM and graph.nodes[r.target_id].qualified_name == "r02::Tr"
    ]
    assert len(inherits) == 1, inherits
    assert graph.nodes[inherits[0].source_id].kind is NodeKind.FILE
    assert "span" in inherits[0].metadata


def test_type_positions_are_queried(tmp_path):
    write_crate(tmp_path, "typos", POSITIONS)
    recorder = Recorder()
    callix.RustAdapter(resolver=recorder).analyze(str(tmp_path))
    seen = Counter(recorder.positions)
    src = POSITIONS

    expected = [
        pos(src, "pub type Alias = ", "Foo"),
        pos(src, "pub struct P(", "Foo"),
        pos(src, ", pub ", "Bar"),
        pos(src, "    A(", "Foo"),
        pos(src, "    B { b: ", "Bar"),
        pos(src, "pub struct W<T: ", "Bound"),
        pos(src, "Bound = ", "Def"),
        pos(src, ">(", "T"),
        pos(src, "pub fn f<U: ", "Bound"),
        pos(src, "(u: ", "U"),
        pos(src, "where ", "Vec"),
        pos(src, "Vec<", "U"),
        pos(src, ">: ", "Bound"),
        pos(src, "let x: ", "Foo"),
        pos(src, "1 as ", "Foo"),
        pos(src, "|a: ", "Foo"),
        pos(src, "-> Bar {", "Bar"),
        pos(src, "impl ", "From"),
        pos(src, "From<", "Foo"),
        pos(src, "for ", "Bar"),
    ]
    for position in expected:
        assert seen[position] == 1, (position, dict(seen))

    # The `T` of `W<T: ..>` and the `U` of `f<U: ..>` declare a parameter.
    w_line, _ = pos(src, "pub struct W<", "T")
    w_decl = (w_line, src.splitlines()[w_line - 1].index("<T") + 2)
    f_line, _ = pos(src, "pub fn f<", "U")
    f_decl = (f_line, src.splitlines()[f_line - 1].index("<U") + 2)
    assert w_decl not in seen, (w_decl, dict(seen))
    assert f_decl not in seen, (f_decl, dict(seen))


def test_impl_header_edges_come_from_the_file(tmp_path):
    write_crate(tmp_path, "hdr", HEADER)
    graph = callix.RustAdapter(resolver=Recorder()).analyze(str(tmp_path))

    def from_file(kind):
        return [
            r
            for r in graph.relations
            if r.kind is kind and graph.nodes[r.source_id].kind is NodeKind.FILE
        ]

    # Ext (self type), Foo (trait argument) and Bar (self type).
    has_type = from_file(RelationKind.HAS_TYPE)
    assert len(has_type) == 3, has_type
    assert all("span" in r.metadata for r in has_type)
    # `impl Tr for Ext`: Ext is not a project class, so the base is the file's.
    inherits = from_file(RelationKind.INHERITS_FROM)
    assert len(inherits) == 1, inherits
    assert "span" in inherits[0].metadata
