"""Every edge a resolver produces says where it was written.

An index of a codebase (LSIF, SCIP) needs a position for each reference, and
the only place a graph keeps one is `metadata["span"]` on the edge. The C family
once built its edges without metadata, which also let `dedupe_structural_relations`
fold two call sites of one function into a single edge — a C or C++ dump had
definitions and hovers and no references.

Only the adapters whose resolver is linked in are covered: they run anywhere.
"""

from __future__ import annotations

import pytest

import callix
from callix import RelationKind
from tests.graphs import FIXTURES

REFERENCE_KINDS = {
    RelationKind.CALLS,
    RelationKind.REFERENCES,
    RelationKind.HAS_TYPE,
    RelationKind.INHERITS_FROM,
}

ADAPTERS = {
    "python": callix.PythonAdapter,
    "typescript": callix.TypeScriptAdapter,
    "php": callix.PhpAdapter,
    "c": callix.CAdapter,
    "cpp": callix.CppAdapter,
}


@pytest.mark.parametrize("language", sorted(ADAPTERS))
def test_resolved_edges_record_their_use_site(language: str) -> None:
    graph = ADAPTERS[language]().analyze(str(FIXTURES / language))
    edges = [r for r in graph.relations if r.kind in REFERENCE_KINDS]
    assert edges, f"{language} fixture produced no reference edges"
    bare = [r for r in edges if "span" not in r.metadata]
    assert not bare, f"{len(bare)} {language} edge(s) record no span, e.g. {bare[0]}"
    assert all(isinstance(r.metadata["span"], callix.Span) for r in edges)


@pytest.mark.parametrize("language", ["c", "cpp"])
def test_reads_and_writes_carry_their_access(language: str) -> None:
    graph = ADAPTERS[language]().analyze(str(FIXTURES / language))
    references = [r for r in graph.relations if r.kind is RelationKind.REFERENCES]
    assert references
    assert {r.metadata.get("access") for r in references} <= {"read", "write"}


OPERATOR_SOURCE = """struct S {
  S& operator=(const S& o) { return *this; }
  bool operator==(const S& o) const { return true; }
  int operator[](int i) const { return i; }
};
bool operator!=(const S& a, const S& b) { return false; }
S make() { return S(); }
int f() {
  S a;
  S b = make();
  a = b;
  bool e = (a == b);
  bool n = (a != b);
  return a[0] + e + n;
}
"""


def test_cpp_operator_edges_cover_the_operator_token(tmp_path) -> None:
    (tmp_path / "a.cc").write_text(OPERATOR_SOURCE)
    graph = callix.CppAdapter().analyze(str(tmp_path))
    names = {i: n.qualified_name for i, n in graph.nodes.items()}
    spans = {}
    for r in graph.relations:
        if r.kind is not RelationKind.CALLS:
            continue
        if not names.get(r.source_id, "").startswith("f("):
            continue
        spans[names.get(r.target_id, "")] = r.metadata["span"]
    for target, line in [
        ("S::operator=(const S&)", 11),
        ("operator!=(const S&,const S&)", 13),
    ]:
        span = spans[target]
        assert isinstance(span, callix.Span)
        assert span.start_line == line
        assert span.start_col <= 5 < span.end_col or line == 13
    assignment = spans["S::operator=(const S&)"]
    assert assignment.start_col <= 5 < assignment.end_col
    assert any(t.startswith("S::operator[]") and s.start_line == 14 for t, s in spans.items())
