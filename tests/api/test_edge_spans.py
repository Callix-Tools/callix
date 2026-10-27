"""Every edge a resolver produces says where it was written.

An index of a codebase (LSIF, SCIP) needs a position for each reference, and
the only place a graph keeps one is `metadata["span"]` on the edge. The C family
once built its edges without metadata, which also let `dedupe_structural_relations`
fold two call sites of one function into a single edge — a C or C++ dump had
definitions and hovers and no references.

Only the adapters whose resolver is linked in are covered: they run anywhere.
"""

from __future__ import annotations

from pathlib import Path

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


def _lib_project(tmp_path: Path, app_source: str) -> Path:
    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "__init__.py").write_text("")
    (tmp_path / "pkg" / "lib.py").write_text(
        "class Widget:\n    pass\n\n\ndef make():\n    return 1\n"
    )
    (tmp_path / "app.py").write_text(app_source)
    return tmp_path


def _span_tuple(relation: object) -> tuple[int, int, int, int]:
    span = relation.metadata["span"]  # type: ignore[attr-defined]
    return (span.start_line, span.start_col, span.end_line, span.end_col)


def test_a_module_level_import_resolves_with_a_span(tmp_path: Path) -> None:
    root = _lib_project(tmp_path, "from pkg.lib import Widget, make\n")
    graph = callix.PythonAdapter().analyze(str(root))
    by_id = {n.id: n for n in graph.nodes.values()}
    found = {}
    for r in graph.relations:
        if r.kind is RelationKind.RESOLVES_TO and "span" in r.metadata:
            found[(by_id[r.source_id].qualified_name, by_id[r.target_id].qualified_name)] = r
    widget = found[("app.Widget", "pkg.lib.Widget")]
    make = found[("app.make", "pkg.lib.make")]
    assert _span_tuple(widget) == (1, 21, 1, 27)
    assert _span_tuple(make) == (1, 29, 1, 33)
    assert widget.metadata["resolution"] == "definition"
    assert make.metadata["resolution"] == "definition"


def test_a_function_scope_import_is_an_import_not_a_use(tmp_path: Path) -> None:
    source = (
        "def f():\n"
        "    from pkg.lib import Widget as W\n"
        "    import pkg.lib as L\n"
        "    return W, L\n"
    )
    root = _lib_project(tmp_path, source)
    graph = callix.PythonAdapter().analyze(str(root))
    by_id = {n.id: n for n in graph.nodes.values()}
    by_name = {n.qualified_name: n for n in graph.nodes.values()}
    # No edge is placed at the aliases `W` (2:35) or `L` (3:23).
    starts = [
        (s.start_line, s.start_col)
        for r in graph.relations
        if isinstance(s := r.metadata.get("span"), callix.Span)
    ]
    assert (2, 35) not in starts
    assert (3, 23) not in starts
    imported = by_name["app.f.W"]
    assert imported.kind is callix.NodeKind.IMPORT
    assert any(
        r.kind is RelationKind.DECLARES
        and r.source_id == by_name["app.f"].id
        and r.target_id == imported.id
        for r in graph.relations
    )
    placed = [
        r
        for r in graph.relations
        if r.kind is RelationKind.RESOLVES_TO
        and r.source_id == imported.id
        and "span" in r.metadata
    ]
    assert [_span_tuple(r) for r in placed] == [(2, 25, 2, 31)]
    assert by_id[placed[0].target_id].qualified_name == "pkg.lib.Widget"
    file_id = by_name["app.py"].id if "app.py" in by_name else next(
        n.id for n in graph.nodes.values() if n.kind is callix.NodeKind.FILE and n.file_path == "app.py"
    )
    assert any(
        r.kind is RelationKind.IMPORTS and r.source_id == file_id for r in graph.relations
    )
    assert not any(
        r.kind is RelationKind.REFERENCES
        and r.source_id == by_name["app.f"].id
        and isinstance(s := r.metadata.get("span"), callix.Span)
        and (s.start_line, s.start_col) == (2, 25)
        for r in graph.relations
    )


@pytest.mark.parametrize("resolve", [True, False])
def test_an_import_inside_a_block_is_an_import(tmp_path: Path, resolve: bool) -> None:
    source = (
        "from typing import TYPE_CHECKING\n"
        "\n"
        "if TYPE_CHECKING:\n"
        "    from pkg.lib import Widget as W\n"
        "\n"
        "try:\n"
        "    from pkg.lib import make\n"
        "except ImportError:\n"
        "    pass\n"
    )
    root = _lib_project(tmp_path, source)
    graph = callix.PythonAdapter(resolve=resolve).analyze(str(root))
    by_name = {n.qualified_name: n for n in graph.nodes.values()}
    for name in ("app.W", "app.make"):
        assert by_name[name].kind is callix.NodeKind.IMPORT, name
    file_id = next(
        n.id for n in graph.nodes.values() if n.kind is callix.NodeKind.FILE and n.file_path == "app.py"
    )
    imported = {
        r.target_id for r in graph.relations if r.kind is RelationKind.IMPORTS and r.source_id == file_id
    }
    assert by_name["app.W"].id in imported or any(
        r.kind is RelationKind.DECLARES and r.target_id == by_name["app.W"].id
        for r in graph.relations
    )
    if resolve:
        spans = {
            by_name["app.W"].id: (4, 25, 4, 31),
            by_name["app.make"].id: (7, 25, 7, 29),
        }
        for node_id, expected in spans.items():
            assert any(
                r.kind is RelationKind.RESOLVES_TO
                and r.source_id == node_id
                and "span" in r.metadata
                and _span_tuple(r) == expected
                for r in graph.relations
            ), node_id
