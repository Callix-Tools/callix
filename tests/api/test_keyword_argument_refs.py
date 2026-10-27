"""A keyword argument's name references the field it sets.

`Item(name=...)` names a dataclass field; `f(a=...)` names a parameter, which is
no edge, and an unknown keyword is nothing at all (and never an external symbol).
"""

from __future__ import annotations

from pathlib import Path

import pytest

import callix
from callix import NodeKind, RelationKind

MODELS = '''\
from dataclasses import dataclass
from typing import dataclass_transform


@dataclass
class Item:
    name: str
    size: int


@dataclass_transform()
class Base:
    pass


class Model(Base):
    c: int


class Plain:
    def __init__(self, d: int) -> None:
        self.d = d


def f(a=1, **kw):
    return a
'''

USE = '''\
from pkg.models import Item, Model, Plain, f


def make():
    Item(name="n", size=1)
    Model(c=3)
    f(a=1, zz=2)
    f(**{})
    Plain(d=4)
    Item(bogus=1)
'''


def _position(keyword: str, nth: int = 0) -> tuple[int, int]:
    """1-based (line, col) of the nth `keyword=` in USE."""
    seen = 0
    for number, line in enumerate(USE.splitlines(), start=1):
        column = line.find(keyword + "=")
        if column >= 0:
            if seen == nth:
                return number, column + 1
            seen += 1
    raise AssertionError(keyword)


@pytest.fixture(scope="module")
def graph(tmp_path_factory: pytest.TempPathFactory) -> callix.Graph:
    root = tmp_path_factory.mktemp("kw")
    (root / "pkg").mkdir()
    (root / "pkg" / "__init__.py").write_text("")
    (root / "pkg" / "models.py").write_text(MODELS)
    (root / "pkg" / "use.py").write_text(USE)
    return callix.PythonAdapter().analyze(str(root))


def _references_at(graph: callix.Graph, line: int, col: int) -> list:
    out = []
    for r in graph.relations:
        span = r.metadata.get("span")
        if r.kind is RelationKind.REFERENCES and span is not None and (span.start_line, span.start_col) == (line, col):
            out.append(r)
    return out


@pytest.mark.parametrize(
    ("keyword", "target"),
    [
        ("name", "pkg.models.Item.name"),
        ("size", "pkg.models.Item.size"),
        ("c", "pkg.models.Model.c"),
    ],
)
def test_dataclass_keyword_references_field(graph: callix.Graph, keyword: str, target: str) -> None:
    by_id = {n.id: n for n in graph.nodes.values()}
    line, col = _position(keyword)
    edges = _references_at(graph, line, col)
    assert len(edges) == 1, edges
    edge = edges[0]
    assert by_id[edge.source_id].qualified_name == "pkg.use.make"
    assert by_id[edge.target_id].qualified_name == target
    assert edge.metadata["access"] == "read"
    assert edge.metadata["resolution"] == "definition"
    span = edge.metadata["span"]
    assert (span.start_line, span.start_col) == (line, col)
    assert span.end_col == col + len(keyword)


@pytest.mark.parametrize("keyword", ["a", "zz", "d", "bogus"])
def test_other_keywords_make_no_edge(graph: callix.Graph, keyword: str) -> None:
    line, col = _position(keyword)
    assert _references_at(graph, line, col) == []
    externals = [
        n
        for n in graph.nodes.values()
        if n.kind is NodeKind.EXTERNAL_SYMBOL and f":{line}:{col}" in n.qualified_name
    ]
    assert externals == []


def test_the_call_edges_are_unchanged(graph: callix.Graph) -> None:
    by_id = {n.id: n for n in graph.nodes.values()}
    calls = {
        (by_id[r.source_id].qualified_name, by_id[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.CALLS
    }
    assert ("pkg.use.make", "pkg.models.Item") in calls
