"""Fixture-project graphs for the `tests/unit` and `tests/api` suites.

Two rules the whole suite depends on:

* nothing here may need a language toolchain — every adapter runs with
  `resolve=False`, so the tests pass on a bare runner (that is also what makes
  them deterministic: no resolver, no timing, no network);
* every graph handed to a test is built fresh, because `Graph` is mutable and
  `merge` / `link_boundaries` / `add_relation` write through it. Analysing a
  fixture project without a resolver costs a few milliseconds.

Plain constructors live in `tests/graphs.py` — import them directly.
"""

from __future__ import annotations

import pytest

import callix
from callix import Graph

from tests.graphs import FIXTURES


@pytest.fixture
def python_graph() -> Graph:
    return callix.PythonAdapter(resolve=False).analyze(FIXTURES / "python")


@pytest.fixture
def yaml_graph() -> Graph:
    return callix.YamlAdapter(resolve=False).analyze(FIXTURES / "yaml")


@pytest.fixture
def typescript_graph() -> Graph:
    return callix.TypeScriptAdapter(resolve=False).analyze(FIXTURES / "typescript")


@pytest.fixture
def go_graph() -> Graph:
    return callix.GoAdapter(resolve=False).analyze(FIXTURES / "go")


@pytest.fixture
def rust_graph() -> Graph:
    return callix.RustAdapter(resolve=False).analyze(FIXTURES / "rust")


@pytest.fixture
def php_graph() -> Graph:
    return callix.PhpAdapter(resolve=False).analyze(FIXTURES / "php")


@pytest.fixture
def cross_language_graph(python_graph: Graph, yaml_graph: Graph) -> Graph:
    """The Python service and the OpenAPI spec in one graph.

    Boundaries are *not* linked yet — `link_boundaries` is the step under test
    wherever this fixture is used.
    """
    python_graph.merge(yaml_graph, allow_shared=True)
    return python_graph
