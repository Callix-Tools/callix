"""`ClangScipResolver` — the opt-in real semantic resolver for C/C++.

Unlike `CFamilyResolver`, this one drives an external tool (`scip-clang`) over
a project's own `compile_commands.json`, so it is not the default and cannot
be exercised end to end without that toolchain. What is tested here needs
neither: the object satisfies the same `prepare`/`resolve_all`/`status`
protocol a custom Python resolver does (`tests/api/test_adapter_api.py`
covers that plumbing generically), and the honest, common case in CI — no
compilation database at all — has to degrade cleanly rather than raise.
"""

from __future__ import annotations

import pathlib

import callix
from callix import ClangScipResolver

from tests.graphs import FIXTURES


def test_repr_before_prepare_is_unavailable():
    assert repr(ClangScipResolver()) == (
        "ClangScipResolver(root=None, documents=0, symbols=0, status=unavailable)"
    )
    assert ClangScipResolver().status().value == "unavailable"


def test_default_compdb_path_is_the_project_root(tmp_path):
    # No compile_commands.json anywhere near this fixture — the common case
    # for a repository that never generated one — so prepare has nothing to
    # index, and that is reported rather than raised.
    resolver = ClangScipResolver()
    resolver.prepare(tmp_path, [])
    assert resolver.status().value == "unavailable"


def test_a_relative_compdb_path_is_resolved_against_project_root(tmp_path):
    resolver = ClangScipResolver(compdb_path="build/compile_commands.json")
    resolver.prepare(tmp_path, [])
    assert resolver.status().value == "unavailable"


def test_an_absolute_compdb_path_is_used_as_is(tmp_path):
    missing = tmp_path / "elsewhere" / "compile_commands.json"
    resolver = ClangScipResolver(compdb_path=missing)
    resolver.prepare(tmp_path, [])
    assert resolver.status().value == "unavailable"


def test_resolve_all_before_prepare_answers_none_for_every_query():
    resolver = ClangScipResolver()
    answers = resolver.resolve_all([(pathlib.Path("a.cc"), 1, 1), (pathlib.Path("b.cc"), 2, 3)])
    assert answers == [None, None]


def test_plugs_into_the_c_and_cpp_adapters_as_a_resolver():
    """No compile_commands.json in the fixture, so this exercises exactly the
    path CI runs: the adapter accepts the resolver, calls it, and reports the
    graph as unavailable rather than failing the analysis."""
    for name, fixture in (("CAdapter", "c"), ("CppAdapter", "cpp")):
        cls = getattr(callix, name)
        graph = cls(resolver=ClangScipResolver()).analyze(FIXTURES / fixture)
        assert graph.metadata["resolver_status"] == "unavailable"
        assert graph.metadata["resolver_metrics"]["queries"] > 0
