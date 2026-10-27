"""Path qualifiers and visibility paths are references (RUST-03).

Only the last segment of a path used to be recorded: `Foo` in `Foo::new()`, `inner`
in `inner::f()` and the `crate` of `pub(crate)` had no occurrence.
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

REPRO = """pub struct Foo;
impl Foo {
    pub fn new() -> Foo { Foo }
}
pub mod inner {
    pub fn f() {}
}
pub fn run() {
    let _a = Foo::new();
    inner::f();
}
pub(crate) fn g() {}
pub(crate) mod m {}
"""

FLAGS_LIB = """pub mod flags;
pub fn run() -> crate::flags::X {
    crate::flags::make()
}
"""

FLAGS_MOD = """pub struct X;
pub fn make() -> X {
    X
}
"""

SUPER = """pub fn g() {}
pub mod inner {
    pub fn h() {
        super::g();
    }
    pub(in crate::inner) fn k() {}
}
"""

SKIPS = """pub struct S;
impl S {
    pub fn len(&self) -> usize {
        let _a = u8::MAX;
        let _b = str::len("x");
        let _c = Self::helper();
        0
    }
    pub fn helper() -> u8 {
        0
    }
    pub fn selfy(&self) {
        self.len();
    }
}
pub mod inner {
    pub fn f() {}
}
use crate::inner::f;
pub fn run() {
    inner::f();
    f();
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
def test_qualifier_and_module_are_references(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": REPRO})
    graph = callix.RustAdapter().analyze(str(tmp_path))

    foo = at(edges(graph, RelationKind.REFERENCES, "demo::run", "demo::Foo"), 9, 14)
    assert graph.nodes[foo.target_id].kind is NodeKind.CLASS
    assert foo.metadata["resolution"] == "definition"
    assert foo.metadata["access"] == "read"

    inner = at(edges(graph, RelationKind.REFERENCES, "demo::run", "demo::inner"), 10, 5)
    assert graph.nodes[inner.target_id].kind is NodeKind.MODULE
    assert inner.metadata["resolution"] == "definition"
    assert inner.metadata["access"] == "read"

    # The calls are still there, at the last segment.
    assert at(edges(graph, RelationKind.CALLS, "demo::run", "demo::Foo.new"), 9, 19)
    assert at(edges(graph, RelationKind.CALLS, "demo::run", "demo::inner::f"), 10, 12)


@needs_rust
def test_pub_crate_refers_to_the_crate_root(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": REPRO})
    graph = callix.RustAdapter().analyze(str(tmp_path))

    g = at(edges(graph, RelationKind.REFERENCES, "demo::g", "demo"), 12, 5)
    assert graph.nodes[g.target_id].kind is NodeKind.MODULE
    assert g.metadata["resolution"] == "definition"

    # A module's visibility is enclosed by the FILE: a MODULE has no file_path.
    from_file = [
        r
        for r in graph.relations
        if r.kind is RelationKind.REFERENCES
        and graph.nodes[r.source_id].kind is NodeKind.FILE
        and graph.nodes[r.target_id].qualified_name == "demo"
        and r.metadata["span"].start_line == 13
    ]
    assert len(from_file) == 1, from_file


@needs_rust
def test_crate_flags_path_in_a_type_and_an_expression(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": FLAGS_LIB, "flags.rs": FLAGS_MOD})
    graph = callix.RustAdapter().analyze(str(tmp_path))

    for hint in ("-> crate", "    crate::flags::make"):
        line, crate_col = pos(FLAGS_LIB, hint, "crate")
        _, flags_col = pos(FLAGS_LIB, hint, "flags")
        at(edges(graph, RelationKind.REFERENCES, "demo::run", "demo"), line, crate_col)
        at(edges(graph, RelationKind.REFERENCES, "demo::run", "demo::flags"), line, flags_col)

    # The last segments keep their own edges.
    line, col = pos(FLAGS_LIB, "-> crate", "X")
    at(edges(graph, RelationKind.HAS_TYPE, "demo::run", "demo::flags::X"), line, col)
    line, col = pos(FLAGS_LIB, "    crate::flags::make", "make")
    at(edges(graph, RelationKind.CALLS, "demo::run", "demo::flags::make"), line, col)


@needs_rust
def test_super_and_pub_in_path(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SUPER})
    graph = callix.RustAdapter().analyze(str(tmp_path))

    line, col = pos(SUPER, "super::g", "super")
    at(edges(graph, RelationKind.REFERENCES, "demo::inner::h", "demo"), line, col)

    line, col = pos(SUPER, "pub(in crate::inner)", "crate")
    at(edges(graph, RelationKind.REFERENCES, "demo::inner::k", "demo"), line, col)
    line, col = pos(SUPER, "pub(in crate::inner)", "inner")
    at(edges(graph, RelationKind.REFERENCES, "demo::inner::k", "demo::inner"), line, col)


def test_every_prefix_is_queried_once(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SKIPS})
    recorder = Recorder()
    callix.RustAdapter(resolver=recorder).analyze(str(tmp_path))
    seen = Counter(recorder.positions)

    # Prefixes of expressions and of `use`-less paths are queried exactly once.
    for want in [
        pos(SKIPS, "let _c = Self::helper", "Self"),
        pos(SKIPS, "    inner::f();", "inner"),
    ]:
        assert seen[want] == 1, (want, sorted(seen.items()))

    # No `self` receiver segment and no primitive qualifier is queried.
    for banned in [
        pos(SKIPS, "let _a = u8::MAX", "u8"),
        pos(SKIPS, "let _b = str::len", "str"),
    ]:
        assert banned not in seen, (banned, sorted(seen.items()))

    # One occurrence per site: no position is asked twice.
    assert all(count == 1 for count in seen.values()), [p for p, c in seen.items() if c > 1]


def test_inline_module_is_a_node(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": SKIPS})
    graph = callix.RustAdapter(resolve=False).analyze(str(tmp_path))

    modules = {n.qualified_name: n for n in graph.nodes.values() if n.kind is NodeKind.MODULE}
    assert "demo::inner" in modules, sorted(modules)
    inner = modules["demo::inner"]
    project = next(n for n in graph.nodes.values() if n.kind is NodeKind.PROJECT)
    assert any(
        r.kind is RelationKind.CONTAINS and r.source_id == project.id and r.target_id == inner.id
        for r in graph.relations
    )
    # `use crate::inner::f` is bound to the MODULE, not to an external symbol.
    assert any(
        r.kind is RelationKind.RESOLVES_TO
        and r.target_id == inner.id
        and graph.nodes[r.source_id].kind is NodeKind.IMPORT
        for r in graph.relations
    )


def test_new_edges_carry_a_span(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": REPRO})
    graph = callix.RustAdapter(resolver=Recorder()).analyze(str(tmp_path))
    refs = [r for r in graph.relations if r.kind is RelationKind.REFERENCES]
    assert refs
    assert all("span" in r.metadata for r in refs)
