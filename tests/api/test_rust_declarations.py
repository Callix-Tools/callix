"""Variants, associated items and in-body items are nodes (RUST-06).

Enum variants and their fields, the associated types and consts of impls and traits, and
the items written in a function body used to have no node, so every answer that landed on
one was an `unlocated` external symbol. A const or static initializer was never scanned.
"""

from __future__ import annotations

import re
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

SRC = """pub enum Kind {
    Plain,
    Named(String),
    Rec { x: u32 },
}

pub enum Code {
    A = 1,
}

pub trait Shape {
    type Out;
    const SIDES: u32 = 4;
}

pub struct Sq;

impl Shape for Sq {
    type Out = u32;
}

pub struct Flags(u32);

impl Flags {
    pub const GET: Self = Self::make(4);
    pub const fn make(b: u32) -> Self { Flags(b) }
}

pub fn helper() -> u32 { 1 }

pub fn run() -> Kind {
    struct Local;
    const N: u32 = helper();
    fn inner() {}
    let _ = Local;
    Kind::Named(String::new())
}
"""


def write_crate(root: Path, name: str, files: dict[str, str]) -> None:
    (root / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\n'
    )
    (root / "src").mkdir()
    for rel, text in files.items():
        (root / "src" / rel).write_text(text)


def pos(src: str, hint: str, ident: str) -> tuple[int, int]:
    """1-based (line, col) of `ident` after `hint` on the first line holding `hint`."""
    for number, text in enumerate(src.splitlines(), start=1):
        if hint in text:
            return number, text.index(ident, text.index(hint)) + 1
    raise AssertionError(hint)


def find(graph, qname, kind):
    hits = [n for n in graph.nodes.values() if n.qualified_name == qname and n.kind is kind]
    assert len(hits) == 1, (qname, kind, hits)
    return hits[0]


def edges(graph, kind, source_qname, target_qname):
    out = []
    for r in graph.relations:
        if r.kind is not kind:
            continue
        if graph.nodes[r.source_id].qualified_name != source_qname:
            continue
        if graph.nodes[r.target_id].qualified_name != target_qname:
            continue
        out.append(r)
    return out


class Table:
    """Answers a lib.rs query by the identifier under it, from a table of definitions."""

    def __init__(self, root: Path, defs: dict[str, tuple[int, int]]) -> None:
        self.root = root
        self.defs = defs
        self.queried: list[tuple[str, int, int]] = []

    def prepare(self, root, files) -> None:
        pass

    def resolve_all(self, queries):
        lines = SRC.splitlines()
        out = []
        for path, line, col in queries:
            if Path(path).name != "lib.rs":
                out.append(None)
                continue
            match = re.match(r"[A-Za-z_][A-Za-z0-9_]*", lines[line - 1][col - 1 :])
            word = match.group(0) if match else ""
            self.queried.append((word, line, col))
            if word in self.defs:
                def_line, def_col = self.defs[word]
                out.append(
                    ResolvedRef(word, str(self.root / "src" / "lib.rs"), def_line, def_col, "function", "internal")
                )
            else:
                out.append(None)
        return out

    def status(self) -> str:
        return "ok"


def test_variants_associated_items_and_body_items_are_nodes(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    graph = callix.RustAdapter(resolve=False).analyze(str(tmp_path))

    cases = [
        ("demo::Kind.Plain", NodeKind.CLASS, pos(SRC, "    Plain,", "Plain")),
        ("demo::Kind.Named", NodeKind.CLASS, pos(SRC, "    Named(String)", "Named")),
        ("demo::Kind.Rec", NodeKind.CLASS, pos(SRC, "    Rec {", "Rec")),
        ("demo::Kind.Rec.x", NodeKind.ATTRIBUTE, pos(SRC, "Rec { x", "x")),
        ("demo::Code.A", NodeKind.CLASS, pos(SRC, "    A = 1", "A")),
        ("demo::Shape.Out", NodeKind.TYPE_ALIAS, pos(SRC, "    type Out;", "Out")),
        ("demo::Shape.SIDES", NodeKind.VARIABLE, pos(SRC, "const SIDES", "SIDES")),
        ("demo::Sq.Out", NodeKind.TYPE_ALIAS, pos(SRC, "    type Out = u32", "Out")),
        ("demo::Flags.GET", NodeKind.VARIABLE, pos(SRC, "const GET", "GET")),
        ("demo::run::Local", NodeKind.CLASS, pos(SRC, "struct Local;", "Local")),
        ("demo::run::N", NodeKind.VARIABLE, pos(SRC, "const N", "N")),
        ("demo::run::inner", NodeKind.FUNCTION, pos(SRC, "fn inner", "inner")),
    ]
    for qname, kind, (line, col) in cases:
        node = find(graph, qname, kind)
        span = node.metadata["name_span"]
        assert (span.start_line, span.start_col) == (line, col), qname

    declares = [
        ("demo::Kind", "demo::Kind.Named"),
        ("demo::Kind", "demo::Kind.Rec"),
        ("demo::Kind.Rec", "demo::Kind.Rec.x"),
        ("demo::Code", "demo::Code.A"),
        ("demo::Shape", "demo::Shape.Out"),
        ("demo::Shape", "demo::Shape.SIDES"),
    ]
    for source, target in declares:
        assert edges(graph, RelationKind.DECLARES, source, target), (source, target)


@needs_rust
def test_enum_payload_types_are_enclosed_by_the_variant(tmp_path):
    # HAS_TYPE edges come from the resolver, so this needs rust-analyzer. A primitive
    # (`u32`) is never an annotation, so only the non-primitive payload `String` is checked.
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    graph = callix.RustAdapter().analyze(str(tmp_path))
    sources = {
        graph.nodes[r.source_id].qualified_name
        for r in graph.relations
        if r.kind is RelationKind.HAS_TYPE
    }
    assert "demo::Kind" not in sources
    assert "demo::Kind.Named" in sources


def test_initializer_and_body_items_own_their_calls(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    defs = {
        "make": pos(SRC, "pub const fn make", "make"),
        "helper": pos(SRC, "pub fn helper", "helper"),
        "Named": pos(SRC, "    Named(String)", "Named"),
        "Local": pos(SRC, "struct Local;", "Local"),
    }
    table = Table(tmp_path, defs)
    graph = callix.RustAdapter(resolver=table).analyze(str(tmp_path))

    get = edges(graph, RelationKind.CALLS, "demo::Flags.GET", "demo::Flags.make")
    assert len(get) == 1
    assert get[0].metadata["resolution"] == "definition"
    line, col = pos(SRC, "Self::make(4)", "make")
    span = get[0].metadata["span"]
    assert (span.start_line, span.start_col) == (line, col)

    # The initializer of a const in a body belongs to the const, never to the function.
    assert edges(graph, RelationKind.CALLS, "demo::run::N", "demo::helper")
    assert not edges(graph, RelationKind.CALLS, "demo::run", "demo::helper")
    assert not edges(graph, RelationKind.CALLS, "demo::Flags.GET", "demo::helper")

    named = edges(graph, RelationKind.CALLS, "demo::run", "demo::Kind.Named")
    assert len(named) == 1
    assert named[0].metadata["resolution"] == "definition"

    local = edges(graph, RelationKind.REFERENCES, "demo::run", "demo::run::Local")
    assert local

    # One site, one query: no double occurrence between the function and its const.
    seen = Counter((line, col) for (_word, line, col) in table.queried)
    for hint, ident in (("Self::make(4)", "make"), ("const N: u32 = helper", "helper")):
        assert seen[pos(SRC, hint, ident)] == 1, hint


@needs_rust
def test_issue_reproduction_with_rust_analyzer(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    graph = callix.RustAdapter().analyze(str(tmp_path))

    unlocated = [
        r
        for r in graph.relations
        if r.kind is RelationKind.CALLS and r.metadata.get("resolution") == "unlocated"
    ]
    assert unlocated == []
    assert edges(graph, RelationKind.CALLS, "demo::Flags.GET", "demo::Flags.make")
    named = edges(graph, RelationKind.CALLS, "demo::run", "demo::Kind.Named")
    assert named and named[0].metadata["resolution"] == "definition"
