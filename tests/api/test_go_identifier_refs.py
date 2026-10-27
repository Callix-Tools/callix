"""A bare use of a package-level const, var or function is a REFERENCES edge."""

from __future__ import annotations

import shutil
from pathlib import Path

import callix
import pytest
from callix import NodeKind, RelationKind

needs_go = pytest.mark.skipif(shutil.which("go") is None, reason="the Go toolchain is not installed")

P = "example.com/m"


def analyse(tmp_path: Path, source: str) -> callix.Graph:
    (tmp_path / "go.mod").write_text("module example.com/m\n\ngo 1.22\n")
    (tmp_path / "a.go").write_text(source)
    return callix.GoAdapter().analyze(str(tmp_path))


def edges(graph: callix.Graph, kinds=(RelationKind.REFERENCES, RelationKind.CALLS)) -> list:
    """(source qname, target qname, kind, access, (line, col)) of every positioned edge."""
    out = []
    for r in graph.relations:
        span = r.metadata.get("span")
        if r.kind in kinds and span is not None:
            out.append(
                (
                    graph.nodes[r.source_id].qualified_name,
                    graph.nodes[r.target_id].qualified_name,
                    r.kind,
                    r.metadata.get("access"),
                    (span.start_line, span.start_col),
                )
            )
    return out


def at(graph: callix.Graph, pos: tuple) -> list:
    return [e for e in edges(graph) if e[4] == pos]


ISSUE = """package m

const limit = 10

var counter int

func use() int {
\tn := limit
\tcounter = n + limit
\treturn counter
}
"""


@needs_go
def test_package_level_identifier_uses_are_references(tmp_path: Path) -> None:
    graph = analyse(tmp_path, ISSUE)
    use = f"{P}.use"
    expected = {
        (8, 7): (f"{P}.limit", "read"),
        (9, 16): (f"{P}.limit", "read"),
        (9, 2): (f"{P}.counter", "write"),
        (10, 9): (f"{P}.counter", "read"),
    }
    for pos, (target, access) in expected.items():
        found = at(graph, pos)
        assert len(found) == 1, (pos, found)
        source, tgt, kind, got_access, _ = found[0]
        assert (source, tgt, kind, got_access) == (use, target, RelationKind.REFERENCES, access)
    for r in graph.relations:
        if r.kind is RelationKind.REFERENCES and r.metadata.get("span") is not None:
            assert r.metadata["resolution"] == "definition"


@needs_go
def test_local_creates_no_node_and_no_external(tmp_path: Path) -> None:
    graph = analyse(tmp_path, ISSUE)
    names = [n.qualified_name for n in graph.nodes.values()]
    assert f"{P}.use.n" not in names
    externals = [n for n in graph.nodes.values() if n.kind is NodeKind.EXTERNAL_SYMBOL]
    assert not [n for n in externals if "@a.go:" in n.qualified_name]
    assert not [e for e in edges(graph) if e[1].endswith(".n")]


@needs_go
def test_shadowing_parameter_or_local_gets_no_edge(tmp_path: Path) -> None:
    source = """package m

var limit = 1

func f(limit int) int {
\treturn limit
}

func g() int {
\tlimit := 1
\treturn limit
}
"""
    graph = analyse(tmp_path, source)
    assert not [e for e in edges(graph) if e[1] == f"{P}.limit"]


@needs_go
def test_builtins_get_no_edge(tmp_path: Path) -> None:
    source = """package m

const base = iota

func use() (bool, []int) {
\tvar x []int
\tx = nil
\tt := true
\treturn t, x
}
"""
    graph = analyse(tmp_path, source)
    assert not [n for n in graph.nodes.values() if n.kind is NodeKind.EXTERNAL_SYMBOL]
    assert edges(graph, (RelationKind.REFERENCES,)) == []


@needs_go
def test_no_double_occurrence(tmp_path: Path) -> None:
    source = """package m

type T struct{ limit int }

var limit = 1

func helper() {}

func h(v int) {}

func use() {
\th(limit)
\thelper()
\t_ = T{limit: 1}
}
"""
    graph = analyse(tmp_path, source)
    seen: dict = {}
    for e in edges(graph):
        if e[0] == f"{P}.use":
            seen.setdefault(e[4], []).append(e)
    for pos, found in seen.items():
        assert len(found) == 1, (pos, found)
    # no self reference from the function's own name
    assert not [e for e in edges(graph) if e[0] == f"{P}.use" and e[1] == f"{P}.use"]
    # h(limit): one REFERENCES to the variable; helper(): one CALLS
    assert [(e[1], e[2]) for e in at(graph, (12, 4))] == [(f"{P}.limit", RelationKind.REFERENCES)]
    assert [(e[1], e[2]) for e in at(graph, (13, 2))] == [(f"{P}.helper", RelationKind.CALLS)]
    # T{limit: 1}: the key is the field, not the package variable
    assert all(e[1] != f"{P}.limit" for e in at(graph, (14, 8)))


@needs_go
def test_selector_operand_and_function_value(tmp_path: Path) -> None:
    source = """package m

import "strings"

var quoteEscaper = strings.NewReplacer("a", "b")

func helper() {}

func use(s string) string {
\th := helper
\t_ = h
\treturn quoteEscaper.Replace(s)
}
"""
    graph = analyse(tmp_path, source)
    operand = [e for e in at(graph, (12, 9)) if e[1] == f"{P}.quoteEscaper"]
    assert [(e[0], e[2], e[3]) for e in operand] == [(f"{P}.use", RelationKind.REFERENCES, "read")]
    value = [e for e in at(graph, (10, 7)) if e[1] == f"{P}.helper"]
    assert [(e[0], e[2], e[3]) for e in value] == [(f"{P}.use", RelationKind.REFERENCES, "read")]
    assert not [e for e in edges(graph) if e[0] == f"{P}.use" and e[1] == f"{P}.helper" and e[2] is RelationKind.CALLS]


@needs_go
def test_package_initializer_reads(tmp_path: Path) -> None:
    source = """package m

const limit = 10

var b = limit * 2
"""
    graph = analyse(tmp_path, source)
    found = [e for e in edges(graph) if e[0] == f"{P}.b" and e[1] == f"{P}.limit"]
    assert [(e[2], e[3]) for e in found] == [(RelationKind.REFERENCES, "read")]


@needs_go
def test_compound_and_incdec_are_writes(tmp_path: Path) -> None:
    source = """package m

var counter int

func use() {
\tcounter++
\tcounter += 1
}
"""
    graph = analyse(tmp_path, source)
    found = [e for e in edges(graph) if e[0] == f"{P}.use" and e[1] == f"{P}.counter"]
    assert [e[3] for e in found] == ["write", "write"]
