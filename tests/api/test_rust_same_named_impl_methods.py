"""Same-named methods of different impls are one node with `declared_at` (RUST-09).

`impl Add<S> for S` and `impl Add<&S> for S` both define `S.add`. The qualified name has
no trait in it, so there is one node; the second declaration's name span is in
`metadata["declared_at"]`, which lets a resolver's answer at it bind to the node.
"""

from __future__ import annotations

import re
from pathlib import Path

import callix
from callix import NodeKind, RelationKind, ResolvedRef

SRC = """use std::ops::Add;
pub struct S(pub i32);
impl Add<S> for S {
    type Output = S;
    fn add(self, r: S) -> S { S(self.0 + r.0) }
}
impl<'a> Add<&'a S> for S {
    type Output = S;
    fn add(self, r: &'a S) -> S { S(self.0 + r.0) }
}
#[cfg(windows)]
impl S {
    pub fn os(&self) -> u32 { 1 }
}
#[cfg(not(windows))]
impl S {
    pub fn os(&self) -> u32 { 2 }
}
#[cfg(windows)]
pub fn plat() -> u32 { 1 }
#[cfg(not(windows))]
pub fn plat() -> u32 { 2 }
pub fn ufcs(a: S, b: S) -> S { <S as Add<&S>>::add(a, &b) }
pub fn outer() {
    struct L;
    impl From<u8> for L {
        fn from(_: u8) -> L { L }
    }
    impl From<u16> for L {
        fn from(_: u16) -> L { L }
    }
}
"""


def write_crate(root: Path, name: str, files: dict[str, str]) -> None:
    (root / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\n'
    )
    (root / "src").mkdir()
    for rel, text in files.items():
        (root / "src" / rel).write_text(text)


def line_of(src: str, hint: str, nth: int = 1) -> int:
    """1-based line of the nth line holding `hint`."""
    seen = 0
    for number, text in enumerate(src.splitlines(), start=1):
        if hint in text:
            seen += 1
            if seen == nth:
                return number
    raise AssertionError(hint)


def pos(src: str, hint: str, ident: str) -> tuple[int, int]:
    for number, text in enumerate(src.splitlines(), start=1):
        if hint in text:
            return number, text.index(ident, text.index(hint)) + 1
    raise AssertionError(hint)


def find(graph, qname, kind):
    hits = [n for n in graph.nodes.values() if n.qualified_name == qname and n.kind is kind]
    assert len(hits) == 1, (qname, kind, hits)
    return hits[0]


def lines_of_node(node) -> list[int]:
    meta = node.metadata
    return [meta["name_span"].start_line] + [s.start_line for s in meta.get("declared_at", [])]


def test_repeated_declarations_are_one_node_with_declared_at(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    graph = callix.RustAdapter(resolve=False).analyze(str(tmp_path))

    add = find(graph, "demo::S.add", NodeKind.METHOD)
    assert add.metadata["name_span"].start_line == line_of(SRC, "fn add", 1)
    assert lines_of_node(add) == [line_of(SRC, "fn add", 1), line_of(SRC, "fn add", 2)]

    os_ = find(graph, "demo::S.os", NodeKind.METHOD)
    assert lines_of_node(os_) == [line_of(SRC, "fn os", 1), line_of(SRC, "fn os", 2)]

    plat = find(graph, "demo::plat", NodeKind.FUNCTION)
    assert lines_of_node(plat) == [line_of(SRC, "fn plat", 1), line_of(SRC, "fn plat", 2)]

    # An item declared once has no declared_at.
    ufcs = find(graph, "demo::ufcs", NodeKind.FUNCTION)
    assert "declared_at" not in ufcs.metadata


def test_same_named_impls_inside_a_body(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    graph = callix.RustAdapter(resolve=False).analyze(str(tmp_path))
    node = find(graph, "demo::outer::L.from", NodeKind.METHOD)
    assert lines_of_node(node) == [line_of(SRC, "fn from", 1), line_of(SRC, "fn from", 2)]


def test_declared_at_survives_a_json_round_trip(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    graph = callix.RustAdapter(resolve=False).analyze(str(tmp_path))
    again = callix.Graph.from_json(graph.to_json())
    add = find(again, "demo::S.add", NodeKind.METHOD)
    assert lines_of_node(add) == [line_of(SRC, "fn add", 1), line_of(SRC, "fn add", 2)]


class Table:
    """Answers a lib.rs query by the identifier under it, from a table of definitions."""

    def __init__(self, root: Path, defs: dict[str, tuple[int, int]]) -> None:
        self.root = root
        self.defs = defs

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


def test_answer_at_the_second_impl_binds_to_the_node(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    # rust-analyzer answers the UFCS call with the SECOND impl's `add`.
    second_add = pos(SRC, "fn add(self, r: &'a S)", "add")
    graph = callix.RustAdapter(resolver=Table(tmp_path, {"add": second_add})).analyze(str(tmp_path))

    hits = [
        r
        for r in graph.relations
        if r.kind is RelationKind.CALLS
        and graph.nodes[r.source_id].qualified_name == "demo::ufcs"
        and graph.nodes[r.target_id].qualified_name == "demo::S.add"
    ]
    assert len(hits) == 1
    assert hits[0].metadata["resolution"] == "definition"
    line, col = pos(SRC, "<S as Add<&S>>::add", "add")
    span = hits[0].metadata["span"]
    assert (span.start_line, span.start_col) == (line, col)
    assert not [
        n for n in graph.nodes.values() if n.kind is NodeKind.EXTERNAL_SYMBOL and n.name.startswith("call@")
    ]
