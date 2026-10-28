"""A `macro_rules!` is a FUNCTION and its invocation is a call (RUST-08).

Also: a std macro nested in another macro's arguments is blanked too, so the call in
`assert_eq!(w, vec!(helper(1)))` is found.
"""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest

import callix
from callix import NodeKind, RelationKind, ResolvedRef

needs_rust = pytest.mark.skipif(
    shutil.which("rust-analyzer") is None or shutil.which("cargo") is None,
    reason="rust-analyzer and cargo are not installed",
)

REPRO = """macro_rules! twice {
    ($e:expr) => {
        $e + $e
    };
}

pub fn helper(x: i32) -> i32 {
    x + 1
}

pub fn run() -> i32 {
    let v = twice!(1);
    assert_eq!(v, 2);
    let w = vec!(helper(0));
    assert_eq!(w, vec!(helper(1)));
    v
}
"""

DOCUMENTED = """/// Doubles.
#[macro_export]
macro_rules! twice {
    ($e:expr) => { $e + $e };
}
mod inner {
    macro_rules! thrice {
        ($e:expr) => { $e + $e + $e };
    }
}
pub fn run() {
    macro_rules! local {
        () => { 1 };
    }
}
"""

INVOKED = """macro_rules! twice {
    ($e:expr) => { $e + $e };
}
macro_rules! make {
    () => { fn made() {} };
}
make!();
pub fn run() -> i32 {
    let a = crate::twice!(1);
    twice!(twice!(2)) + a
}
"""

ITEM_LEVEL = """macro_rules! make {
    () => { fn made() {} };
}
make!();
pub fn run() {
    made();
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


def node(graph, qname):
    found = [n for n in graph.nodes.values() if n.qualified_name == qname]
    assert found, (qname, sorted(n.qualified_name for n in graph.nodes.values()))
    return found[0]


def calls(graph, source_qname, target_qname):
    out = []
    for r in graph.relations:
        if r.kind is not RelationKind.CALLS:
            continue
        if graph.nodes[r.source_id].qualified_name != source_qname:
            continue
        if graph.nodes[r.target_id].qualified_name != target_qname:
            continue
        out.append(r)
    return out


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


def test_macro_rules_is_a_function(tmp_path):
    write_crate(tmp_path, "rust08", {"lib.rs": REPRO})
    graph = callix.RustAdapter(resolve=False).analyze(str(tmp_path))
    twice = node(graph, "rust08::twice")
    assert twice.kind is NodeKind.FUNCTION
    assert twice.metadata["macro"] == "macro_rules"
    name_span = twice.metadata["name_span"]
    assert (name_span.start_line, name_span.start_col) == (1, 14)
    declares = [
        r
        for r in graph.relations
        if r.kind is RelationKind.DECLARES
        and r.target_id == twice.id
        and graph.nodes[r.source_id].kind is NodeKind.FILE
    ]
    assert len(declares) == 1


def test_macro_doc_module_and_body_scopes(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": DOCUMENTED})
    graph = callix.RustAdapter(resolve=False).analyze(str(tmp_path))
    assert node(graph, "demo::twice").metadata["doc"].strip() == "Doubles."
    assert node(graph, "demo::inner::thrice").metadata["macro"] == "macro_rules"
    assert node(graph, "demo::run::local").metadata["macro"] == "macro_rules"


def test_invocation_names_are_queried(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": INVOKED})
    recorder = Recorder()
    callix.RustAdapter(resolver=recorder).analyze(str(tmp_path))
    wanted = [
        pos(INVOKED, "make!();", "make"),  # item level
        pos(INVOKED, "crate::twice!(1)", "twice"),  # the name, not `crate`
        pos(INVOKED, "twice!(twice!(2))", "twice"),
    ]
    line, col = pos(INVOKED, "twice!(twice!(2))", "twice!(2)")
    wanted.append((line, col))  # the nested one, written in the arguments
    for position in wanted:
        assert position in recorder.positions, (position, recorder.positions)


def test_nested_std_macro_argument_is_queried(tmp_path):
    write_crate(tmp_path, "rust08", {"lib.rs": REPRO})
    recorder = Recorder()
    callix.RustAdapter(resolver=recorder).analyze(str(tmp_path))
    assert pos(REPRO, "assert_eq!(w, vec!(helper(1)))", "helper") in recorder.positions
    assert pos(REPRO, "let v = twice!(1)", "twice") in recorder.positions


@needs_rust
def test_issue_repro_end_to_end(tmp_path):
    write_crate(tmp_path, "rust08", {"lib.rs": REPRO})
    graph = callix.RustAdapter().analyze(str(tmp_path))
    twice = calls(graph, "rust08::run", "rust08::twice")
    assert [r.metadata["span"].start_line for r in twice] == [12]
    assert twice[0].metadata["resolution"] == "definition"
    helper = calls(graph, "rust08::run", "rust08::helper")
    assert sorted(r.metadata["span"].start_line for r in helper) == [14, 15]
    assert all(r.metadata["resolution"] == "definition" for r in helper)


@needs_rust
def test_item_level_invocation_is_enclosed_by_the_file(tmp_path):
    write_crate(tmp_path, "demo", {"lib.rs": ITEM_LEVEL})
    graph = callix.RustAdapter().analyze(str(tmp_path))
    make = node(graph, "demo::make")
    from_file = [
        r
        for r in graph.relations
        if r.kind is RelationKind.CALLS
        and r.target_id == make.id
        and graph.nodes[r.source_id].kind is NodeKind.FILE
        and r.metadata["span"].start_line == 4
    ]
    assert len(from_file) == 1


@needs_rust
def test_macro_body_name_fallback_skips_macro_nodes(tmp_path):
    src = (
        "fn step() {}\n"
        "macro_rules! step_twice {\n    () => { step(); step(); };\n}\n"
        "macro_rules! step {\n    () => {};\n}\n"
        "pub fn run() {\n    step_twice!();\n}\n"
    )
    write_crate(tmp_path, "demo", {"lib.rs": src})
    graph = callix.RustAdapter().analyze(str(tmp_path))
    assert calls(graph, "demo::run", "demo::step"), "the fn must still bind by name"
