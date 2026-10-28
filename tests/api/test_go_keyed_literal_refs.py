"""The key of a keyed struct literal is a write of the field it names."""

from __future__ import annotations

import shutil
from pathlib import Path

import callix
import pytest
from callix import RelationKind

needs_go = pytest.mark.skipif(shutil.which("go") is None, reason="the Go toolchain is not installed")

P = "example.com/m"


def analyse(tmp_path: Path, source: str) -> callix.Graph:
    (tmp_path / "go.mod").write_text("module example.com/m\n\ngo 1.22\n")
    (tmp_path / "a.go").write_text(source)
    return callix.GoAdapter().analyze(str(tmp_path))


def refs(graph: callix.Graph) -> list:
    return [
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name, r)
        for r in graph.relations
        if r.kind is RelationKind.REFERENCES
    ]


def writes(graph: callix.Graph) -> list:
    return [(s, t, r) for s, t, r in refs(graph) if r.metadata.get("access") == "write"]


BASIC = """package m

type T struct {
\tName string
\tn    int
}

func New(v string) T {
\treturn T{Name: v, n: 1}
}
"""


@needs_go
def test_keyed_struct_literal_key_references_field(tmp_path: Path) -> None:
    graph = analyse(tmp_path, BASIC)
    found = {(s, t): r for s, t, r in writes(graph)}
    for target, line, col in [(f"{P}.T.Name", 9, 11), (f"{P}.T.n", 9, 20)]:
        relation = found[(f"{P}.New", target)]
        span = relation.metadata["span"]
        assert (span.start_line, span.start_col) == (line, col)
        assert relation.metadata["access"] == "write"
        assert relation.metadata["resolution"] == "definition"


NESTED = """package m

type T struct{ Name string }

type Outer struct{ In T }

func A(v string) *T { return &T{Name: v} }

func B(v string) Outer { return Outer{In: T{Name: v}} }

func C(v string) []T { return []T{{Name: v}} }

func D(v string) []*T { return []*T{{Name: v}} }

func E(v string) map[string]T { return map[string]T{"a": {Name: v}} }
"""


@needs_go
def test_pointer_nested_and_elided_literals(tmp_path: Path) -> None:
    graph = analyse(tmp_path, NESTED)
    found = {(s, t) for s, t, _ in writes(graph)}
    for fn in "ACDE":
        assert (f"{P}.{fn}", f"{P}.T.Name") in found, fn
    assert (f"{P}.B", f"{P}.Outer.In") in found
    assert (f"{P}.B", f"{P}.T.Name") in found


@needs_go
def test_package_level_var_literal(tmp_path: Path) -> None:
    source = "package m\n\ntype T struct{ Name string }\n\nvar Default = T{Name: \"x\"}\n"
    graph = analyse(tmp_path, source)
    found = {(s, t) for s, t, _ in writes(graph)}
    assert (f"{P}.Default", f"{P}.T.Name") in found


NEGATIVE = """package m

type M map[string]int

const idx = 1

func F(k string) map[string]int { return map[string]int{k: 1} }

func G(k string) M { return M{k: 1} }

func H() []string { return [...]string{idx: "a"}[:] }

func I() map[bool]int { return map[bool]int{true: 1} }
"""


@needs_go
def test_map_and_index_keys_give_no_field_edge(tmp_path: Path) -> None:
    graph = analyse(tmp_path, NEGATIVE)
    assert writes(graph) == []
    assert not [n for n in graph.nodes.values() if n.qualified_name.startswith("write@")]


@needs_go
def test_external_struct_field_key(tmp_path: Path) -> None:
    source = (
        'package m\n\nimport "net/http"\n\n'
        'func S() *http.Server { return &http.Server{Addr: ":80"} }\n'
    )
    graph = analyse(tmp_path, source)
    found = [r for s, _, r in writes(graph) if s == f"{P}.S"]
    assert len(found) == 1
    assert found[0].metadata["resolution"] == "external"
