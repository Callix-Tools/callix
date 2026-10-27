"""Go package qualifiers and import paths: a `pkg.X` / `pkg.T` qualifier is a
REFERENCES edge to the package's MODULE (or to the import's external symbol), the
import path literal is the span of the IMPORT's RESOLVES_TO, and a MODULE is
placed at the package clause of its first non-test file."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import callix
import pytest
from callix import NodeKind, RelationKind

needs_go = pytest.mark.skipif(shutil.which("go") is None, reason="the Go toolchain is not installed")

REPO = Path(__file__).resolve().parents[2]
UTIL = "example.com/m/util"
STRS = "example.com/m/strs"

UTIL_GO = "package util\n\ntype T struct{ N int }\n\nfunc Double(x int) int { return x * 2 }\n"
STRS_GO = "package strs\n\nfunc Up(s string) string { return s }\n"
TEST_GO = "package util\n\nimport \"testing\"\n\nfunc TestDouble(t *testing.T) {}\n"
MAIN_GO = """package main

import "example.com/m/util"
import s "example.com/m/strs"
import "fmt"

func main() {
    _ = util.Double(2)
    _ = s.Up("a")
    fmt.Println("x")
}

func param(t util.T) {}

func shadow() {
    util := struct{ X int }{}
    _ = util.X
}
"""

# The layout of the issue: the qualifier is at 6:6 and the import path at 3:8.
REPRO_UTIL = "package util\n\nfunc Double(x int) int { return x * 2 }\n"
REPRO_MAIN = 'package main\n\nimport "example.com/m/util"\n\nfunc main() {\n\t_ = util.Double(2)\n}\n'


def write_project(root: Path, main: str, extra: dict[str, str]) -> None:
    (root / "go.mod").write_text("module example.com/m\n\ngo 1.22\n")
    (root / "main.go").write_text(main)
    for name, text in extra.items():
        (root / name).parent.mkdir(parents=True, exist_ok=True)
        (root / name).write_text(text)


def loc(src: str, marker: str, word: str) -> tuple[int, int]:
    """1-based (line, col) of `word` on the first line containing `marker`."""
    for number, line in enumerate(src.split("\n"), start=1):
        if marker in line:
            return number, line.index(word) + 1
    raise AssertionError(f"no line contains {marker!r}")


def start(relation: callix.Relation) -> tuple[int, int]:
    span = relation.metadata["span"]
    return span.start_line, span.start_col


def refs_to(graph: callix.Graph, target: str) -> list[callix.Relation]:
    return [
        r
        for r in graph.relations
        if r.kind is RelationKind.REFERENCES and graph.nodes[r.target_id].qualified_name == target
    ]


@needs_go
def test_package_qualifier_and_import_path_have_spans(tmp_path: Path) -> None:
    write_project(tmp_path, MAIN_GO, {"util/util.go": UTIL_GO, "strs/strs.go": STRS_GO})
    graph = callix.GoAdapter().analyze(str(tmp_path))

    util = {(graph.nodes[r.source_id].qualified_name, *start(r)) for r in refs_to(graph, UTIL)}
    # `util.Double(2)`: the qualifier of a call.
    assert ("example.com/m.main", *loc(MAIN_GO, "util.Double", "util")) in util
    # `t util.T`: the qualifier of a type, enclosed by the parameter.
    assert ("example.com/m.param.t", *loc(MAIN_GO, "func param", "util")) in util
    # `util := ...; util.X`: a local that shadows the package gets no edge.
    assert not [e for e in util if e[0] == "example.com/m.shadow"], util

    (call,) = [r for r in refs_to(graph, UTIL) if graph.nodes[r.source_id].qualified_name == "example.com/m.main"]
    assert graph.nodes[call.target_id].kind is NodeKind.MODULE
    assert call.metadata["resolution"] == "definition"
    assert call.metadata["access"] == "read"

    # An aliased import: the qualifier is the alias.
    strs = {(graph.nodes[r.source_id].qualified_name, *start(r)) for r in refs_to(graph, STRS)}
    assert ("example.com/m.main", *loc(MAIN_GO, "s.Up", "s")) in strs

    # A standard-library package: the import's external symbol.
    (fmt,) = refs_to(graph, "fmt")
    assert graph.nodes[fmt.target_id].kind is NodeKind.EXTERNAL_SYMBOL
    assert fmt.metadata["resolution"] == "external"
    assert start(fmt) == loc(MAIN_GO, "fmt.Println", "fmt")

    # The import path literal, quotes included, is the span of RESOLVES_TO.
    literal = '"example.com/m/util"'
    line, col = loc(MAIN_GO, 'import "example.com/m/util"', literal)
    (resolves,) = [
        r
        for r in graph.relations
        if r.kind is RelationKind.RESOLVES_TO and graph.nodes[r.target_id].qualified_name == UTIL
    ]
    span = resolves.metadata["span"]
    assert (span.start_line, span.start_col, span.end_line, span.end_col) == (line, col, line, col + len(literal))


@needs_go
def test_issue_repro_spans(tmp_path: Path) -> None:
    write_project(tmp_path, REPRO_MAIN, {"util/util.go": REPRO_UTIL})
    graph = callix.GoAdapter().analyze(str(tmp_path))
    (ref,) = refs_to(graph, UTIL)
    span = ref.metadata["span"]
    assert (span.start_line, span.start_col, span.end_line, span.end_col) == (6, 6, 6, 10)
    (resolves,) = [
        r
        for r in graph.relations
        if r.kind is RelationKind.RESOLVES_TO and graph.nodes[r.target_id].qualified_name == UTIL
    ]
    span = resolves.metadata["span"]
    assert (span.start_line, span.start_col, span.end_line, span.end_col) == (3, 8, 3, 28)


def test_import_path_span_and_package_placement_without_resolution(tmp_path: Path) -> None:
    write_project(
        tmp_path,
        MAIN_GO,
        {"util/util.go": UTIL_GO, "util/a_test.go": TEST_GO, "strs/strs.go": STRS_GO},
    )
    graph = callix.GoAdapter(resolve=False).analyze(str(tmp_path))

    # No resolver, no qualifier edges.
    assert not [r for r in graph.relations if r.kind is RelationKind.REFERENCES]
    # The import path needs no resolver.
    resolves = [
        r
        for r in graph.relations
        if r.kind is RelationKind.RESOLVES_TO and graph.nodes[r.target_id].qualified_name == UTIL
    ]
    assert len(resolves) == 1 and "span" in resolves[0].metadata
    # The package is placed on its first non-test file, at the package clause.
    (module,) = [n for n in graph.nodes.values() if n.qualified_name == UTIL]
    assert module.kind is NodeKind.MODULE
    assert module.file_path == "util/util.go"
    assert module.span is None
    name = module.metadata["name_span"]
    assert (name.start_line, name.start_col, name.end_line, name.end_col) == (1, 9, 1, 13)


def _cli() -> Path | None:
    override = os.environ.get("CALLIX_CLI")
    if override:
        return Path(override)
    for profile in ("release", "debug"):
        candidate = REPO / "target" / profile / "callix"
        if candidate.exists():
            return candidate
    return None


@needs_go
@pytest.mark.skipif(_cli() is None, reason="build the CLI first: task cli")
def test_package_exports_to_lsif(tmp_path: Path) -> None:
    write_project(tmp_path, MAIN_GO, {"util/util.go": UTIL_GO, "strs/strs.go": STRS_GO})
    graph = callix.GoAdapter().analyze(str(tmp_path))
    dump = tmp_path / "g.json"
    dump.write_text(graph.to_json())
    out = tmp_path / "g.lsif"
    subprocess.run(
        [str(_cli()), "export", str(dump), "--source-root", str(tmp_path), "-f", "lsif", "-o", str(out)],
        check=True,
        capture_output=True,
    )
    starts = set()
    for text in out.read_text().splitlines():
        element = json.loads(text)
        if element.get("type") == "vertex" and element.get("label") == "range":
            starts.add((element["start"]["line"], element["start"]["character"]))
    qualifier = loc(MAIN_GO, "util.Double", "util")
    literal = loc(MAIN_GO, 'import "example.com/m/util"', '"example.com/m/util"')
    # 0-based: the package definition, the import path and the qualifier.
    assert (0, 8) in starts
    assert (literal[0] - 1, literal[1] - 1) in starts
    assert (qualifier[0] - 1, qualifier[1] - 1) in starts
