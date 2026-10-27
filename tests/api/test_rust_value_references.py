"""Values and patterns are references (RUST-05).

A const read, a struct literal, a shorthand initializer and the paths and fields of a
pattern used to have no occurrence at all. A read that lands on a local or a parameter
is not an edge.
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

SRC = """pub enum Mode { A, B(u32) }
pub const LIMIT: u32 = 3;
pub struct Cfg { pub n: u32 }
pub fn n_fn() -> u32 { 1 }

pub fn lim() -> u32 {
    LIMIT
}

pub fn build() -> Cfg {
    Cfg { n: 1 }
}

pub fn short(n: u32) -> Cfg {
    Cfg { n }
}

pub fn pick(m: Mode) -> u32 {
    match m {
        Mode::B(x) => x,
        Mode::A => LIMIT,
    }
}

pub fn unpack(c: Cfg) -> u32 {
    let Cfg { n } = c;
    n
}

pub fn shadow() -> u32 {
    let n_fn = 2;
    n_fn
}

pub fn twice() -> u32 {
    let a = LIMIT + 1;
    a
}

pub fn id(v: u32) -> u32 {
    v
}

pub fn run() -> u32 {
    let k = 1;
    id(k)
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


def edges(graph, kind, source_qname, target_qname):
    """[(line, col, relation)] of the edges of a kind between two qualified names."""
    out = []
    for r in graph.relations:
        if r.kind is not kind:
            continue
        if graph.nodes[r.source_id].qualified_name != source_qname:
            continue
        if graph.nodes[r.target_id].qualified_name != target_qname:
            continue
        span = r.metadata["span"]
        out.append((span.start_line, span.start_col, r))
    return out


def at(found, line, col):
    hits = [r for (l, c, r) in found if (l, c) == (line, col)]
    assert hits, (line, col, [(l, c) for (l, c, _) in found])
    return hits[0]


def refs_at(graph, source_qname, line, col):
    """Every REFERENCES edge from a function whose span starts at (line, col)."""
    return [
        r
        for r in graph.relations
        if r.kind is RelationKind.REFERENCES
        and graph.nodes[r.source_id].qualified_name == source_qname
        and (r.metadata["span"].start_line, r.metadata["span"].start_col) == (line, col)
    ]


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
def test_values_and_patterns_are_references(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    graph = callix.RustAdapter().analyze(str(tmp_path))

    cases = [
        ("demo::lim", "demo::LIMIT", pos(SRC, "    LIMIT", "LIMIT")),
        ("demo::build", "demo::Cfg", pos(SRC, "Cfg { n: 1 }", "Cfg")),
        ("demo::build", "demo::Cfg.n", pos(SRC, "Cfg { n: 1 }", "n")),
        ("demo::short", "demo::Cfg.n", pos(SRC, "    Cfg { n }", "n")),
        ("demo::pick", "demo::LIMIT", pos(SRC, "Mode::A =>", "LIMIT")),
        ("demo::unpack", "demo::Cfg", pos(SRC, "let Cfg { n }", "Cfg")),
        ("demo::unpack", "demo::Cfg.n", pos(SRC, "let Cfg { n }", "n")),
    ]
    for source, target, (line, col) in cases:
        edge = at(edges(graph, RelationKind.REFERENCES, source, target), line, col)
        assert edge.metadata["access"] == "read", (source, target)
        assert edge.metadata["resolution"] == "definition", (source, target)


@needs_rust
def test_variant_paths_in_a_pattern_have_an_edge_with_a_span(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    graph = callix.RustAdapter().analyze(str(tmp_path))

    # Variants are not nodes yet (RUST-06): the target kind is deliberately not pinned.
    for hint, ident in (("Mode::B(x)", "B"), ("Mode::A =>", "A")):
        line, col = pos(SRC, hint, ident)
        found = refs_at(graph, "demo::pick", line, col)
        assert found, (hint, line, col)
        assert all("span" in r.metadata for r in found)


@needs_rust
def test_locals_and_parameters_are_not_edges(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    graph = callix.RustAdapter().analyze(str(tmp_path))

    # A local named like a function does not reference the function.
    assert not [r for r in graph.relations if r.kind is RelationKind.REFERENCES
                and graph.nodes[r.source_id].qualified_name == "demo::shadow"
                and graph.nodes[r.target_id].qualified_name == "demo::n_fn"]
    line, col = pos(SRC, "    n_fn", "n_fn")
    assert refs_at(graph, "demo::shadow", line, col) == []

    # The reads of `m`, `x`, `c` and the local `n` have no edge of any kind.
    for source, line_text, ident in (
        ("demo::pick", "match m {", "m"),
        ("demo::pick", "Mode::B(x) => x", "x"),
    ):
        line, col = pos(SRC, line_text, ident)
        if ident == "x":
            col = SRC.splitlines()[line - 1].rindex("x") + 1
        assert refs_at(graph, source, line, col) == [], (source, line, col)
    line, col = pos(SRC, "let Cfg { n } = c;", "c;")
    assert refs_at(graph, "demo::unpack", line, col) == []

    # No REFERENCES edge anywhere ends on a PARAMETER.
    for r in graph.relations:
        if r.kind is RelationKind.REFERENCES:
            assert graph.nodes[r.target_id].kind is not NodeKind.PARAMETER, r


def test_each_value_site_is_queried_once(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    recorder = Recorder()
    callix.RustAdapter(resolver=recorder).analyze(str(tmp_path))
    seen = Counter(recorder.positions)

    once = [
        pos(SRC, "let a = LIMIT", "LIMIT"),
        pos(SRC, "Cfg { n: 1 }", "Cfg"),
        pos(SRC, "Cfg { n: 1 }", "n"),
        pos(SRC, "Mode::B(x)", "B"),
        # The callee and the plain-name argument keep their own occurrence only.
        pos(SRC, "    id(k)", "id"),
        pos(SRC, "    id(k)", "k"),
    ]
    for want in once:
        assert seen[want] == 1, (want, sorted(seen.items()))

    # A shorthand initializer is queried (a flow occurrence may share its position).
    assert seen[pos(SRC, "    Cfg { n }", "n")] >= 1

    # A lowercase binding in a pattern is never queried.
    assert pos(SRC, "Mode::B(x)", "x") not in seen


def test_value_edges_carry_a_span(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SRC})
    graph = callix.RustAdapter(resolver=Recorder()).analyze(str(tmp_path))
    refs = [r for r in graph.relations if r.kind is RelationKind.REFERENCES]
    assert refs
    assert all("span" in r.metadata for r in refs)
