"""Names in a `use` tree and a bodyless `mod x;` have positioned edges (RUST-04).

The IMPORT stays one node per declaration. Each name it brings in is a RESOLVES_TO
from the IMPORT with a span, and `mod x;` is a REFERENCES from the FILE to the MODULE.
Both are bound by path in the structure pass (`resolution` `name`), so they exist
with `resolve=False`, and a resolver is asked only about what was not bound.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind, ResolvedRef

LIB = """mod util;
pub mod shapes;

use crate::shapes::{Circle, Square};

pub fn run() -> Circle {
    util::helper();
    let _s: Option<Square> = None;
    Circle
}
"""
UTIL = "pub fn helper() {}\n"
SHAPES = "pub struct Circle;\npub struct Square;\n"


def write_crate(root: Path, files: dict[str, str]) -> None:
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\n'
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


def spanned(graph, kind, source_kind):
    """{(line, col): [relation, ...]} of the edges of a kind that record a position."""
    out: dict[tuple[int, int], list] = {}
    for r in graph.relations:
        if r.kind is not kind or graph.nodes[r.source_id].kind is not source_kind:
            continue
        span = r.metadata.get("span")
        if span is None:
            continue
        out.setdefault((span.start_line, span.start_col), []).append(r)
    return out


def leaves(graph):
    return spanned(graph, RelationKind.RESOLVES_TO, NodeKind.IMPORT)


def target(graph, relation) -> str:
    return graph.nodes[relation.target_id].qualified_name


def analyse(tmp_path, files, **kwargs):
    write_crate(tmp_path, files)
    return callix.RustAdapter(**kwargs).analyze(str(tmp_path))


def test_use_tree_names_and_mod_decl_have_positioned_edges(tmp_path):
    graph = analyse(
        tmp_path, {"lib.rs": LIB, "util.rs": UTIL, "shapes.rs": SHAPES}, resolve=False
    )
    found = leaves(graph)
    circle = pos(LIB, "use crate::shapes", "Circle")
    square = pos(LIB, "use crate::shapes", "Square")
    assert circle == (4, 21) and square == (4, 29)
    assert target(graph, found[circle][0]) == "demo::shapes::Circle"
    assert target(graph, found[square][0]) == "demo::shapes::Square"
    for key in (circle, square):
        assert len(found[key]) == 1
        assert found[key][0].metadata["resolution"] == "name"

    modules = spanned(graph, RelationKind.REFERENCES, NodeKind.FILE)
    util = modules[(1, 5)][0]
    assert graph.nodes[util.source_id].qualified_name == "src/lib.rs"
    assert graph.nodes[util.target_id].kind is NodeKind.MODULE
    assert target(graph, util) == "demo::util"
    assert target(graph, modules[(2, 9)][0]) == "demo::shapes"
    for key in ((1, 5), (2, 9)):
        assert modules[key][0].metadata["resolution"] == "name"
        assert modules[key][0].metadata["access"] == "read"

    # The structural edges, which carry no position, are still there.
    structural = [
        r
        for r in graph.relations
        if r.kind in (RelationKind.IMPORTS, RelationKind.RESOLVES_TO)
        and target(graph, r) == "demo::shapes"
        and r.metadata.get("span") is None
    ]
    assert {r.kind for r in structural} == {RelationKind.IMPORTS, RelationKind.RESOLVES_TO}


def test_use_as_clause_points_at_the_original_name(tmp_path):
    lib = "pub mod shapes;\nuse crate::shapes::Circle as Round;\n"
    graph = analyse(tmp_path, {"lib.rs": lib, "shapes.rs": SHAPES}, resolve=False)
    found = leaves(graph)
    circle = pos(lib, "use crate", "Circle")
    round_ = pos(lib, " as ", "Round")
    assert target(graph, found[circle][0]) == "demo::shapes::Circle"
    assert round_ not in found
    assert len(found) == 1


def test_nested_groups(tmp_path):
    lib = (
        "pub mod a {\n"
        "    pub mod b { pub struct C; pub struct D; }\n"
        "    pub struct E;\n"
        "}\n"
        "use crate::a::{b::{C, D}, E};\n"
    )
    graph = analyse(tmp_path, {"lib.rs": lib}, resolve=False)
    found = leaves(graph)
    for name, qname in (
        ("C", "demo::a::b::C"),
        ("D", "demo::a::b::D"),
        ("E", "demo::a::E"),
    ):
        assert target(graph, found[pos(lib, "use crate::a", name)][0]) == qname


def test_skipped_and_unbound_leaves(tmp_path):
    lib = (
        "pub mod shapes;\n"
        "use crate::shapes::*;\n"
        "use crate::shapes::{self};\n"
        "use std::fmt::Display;\n"
    )
    graph = analyse(tmp_path, {"lib.rs": lib, "shapes.rs": SHAPES}, resolve=False)
    found = leaves(graph)
    lines = {line for (line, _col) in found}
    assert lines.isdisjoint({2, 3, 4}), found
    # The structural edge of the external import is unchanged.
    assert any(
        r.kind is RelationKind.IMPORTS and target(graph, r) == "std::fmt::Display"
        for r in graph.relations
    )

    reexport = (
        "pub mod b { pub struct X; }\n"
        "pub mod a { pub use crate::b::X; }\n"
        "use crate::a::X;\n"
    )
    other = tmp_path / "second"
    other.mkdir()
    graph = analyse(other, {"lib.rs": reexport}, resolve=False)
    found = leaves(graph)
    inner = pos(reexport, "pub use", "X")
    outer = pos(reexport, "use crate::a", "X")
    assert target(graph, found[inner][0]) == "demo::b::X"
    assert outer not in found


def test_relative_and_super_paths(tmp_path):
    lib = (
        "mod shapes;\n"
        "use shapes::Circle;\n"
        "pub mod inner {\n"
        "    use super::f;\n"
        "}\n"
        "pub fn f() {}\n"
    )
    graph = analyse(tmp_path, {"lib.rs": lib, "shapes.rs": SHAPES}, resolve=False)
    found = leaves(graph)
    relative = found[pos(lib, "use shapes", "Circle")][0]
    assert target(graph, relative) == "demo::shapes::Circle"
    assert relative.metadata["resolution"] == "name"
    up = found[pos(lib, "use super", "f")][0]
    assert target(graph, up) == "demo::f"
    assert up.metadata["resolution"] == "name"


class Recorder:
    """Records every (line, col) queried in lib.rs; answers with a fixed reference."""

    def __init__(self, full_name: str, origin: str) -> None:
        self.full_name = full_name
        self.origin = origin
        self.positions: list[tuple[int, int]] = []

    def prepare(self, root, files) -> None:
        pass

    def resolve_all(self, queries):
        for path, line, col in queries:
            if Path(path).name == "lib.rs":
                self.positions.append((line, col))
        return [
            ResolvedRef(self.full_name, None, 1, 1, "struct", self.origin) for _ in queries
        ]

    def status(self) -> str:
        return "ok"


RESOLVER_LIB = LIB.replace(
    "use crate::shapes::{Circle, Square};\n",
    "use crate::shapes::{Circle, Square};\nuse std::fmt::Display;\n",
)


def test_no_duplicate_with_a_resolver(tmp_path):
    recorder = Recorder("spied.Foo", "third_party")
    graph = analyse(
        tmp_path,
        {"lib.rs": RESOLVER_LIB, "util.rs": UTIL, "shapes.rs": SHAPES},
        resolver=recorder,
    )
    circle = pos(RESOLVER_LIB, "use crate::shapes", "Circle")
    square = pos(RESOLVER_LIB, "use crate::shapes", "Square")
    display = pos(RESOLVER_LIB, "use std::fmt", "Display")
    # Bound leaves and `mod x;` are never queried; the external leaf exactly once.
    for bound in (circle, square, (1, 5), (2, 9)):
        assert bound not in recorder.positions, bound
    assert recorder.positions.count(display) == 1
    found = leaves(graph)
    for key in (circle, square, display):
        assert len(found[key]) == 1, key
    answered = found[display][0]
    assert graph.nodes[answered.target_id].kind is NodeKind.EXTERNAL_SYMBOL
    assert target(graph, answered) == "spied.Foo"
    assert answered.metadata["resolution"] == "external"


def test_an_unbound_answer_with_no_name_gives_no_edge(tmp_path):
    graph = analyse(
        tmp_path,
        {"lib.rs": RESOLVER_LIB, "util.rs": UTIL, "shapes.rs": SHAPES},
        resolver=Recorder("", "unknown"),
    )
    display = pos(RESOLVER_LIB, "use std::fmt", "Display")
    assert display not in leaves(graph)
