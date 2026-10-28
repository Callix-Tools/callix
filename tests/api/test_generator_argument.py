"""A generator expression that is the only argument of a call is scanned.

tree-sitter stores `f(x for x in xs)` with the generator in place of an
`argument_list`; the visitor once looked only for the latter, so nothing inside
the generator was visited while the list-comprehension form was.
"""

from __future__ import annotations

from collections import Counter
from pathlib import Path

import callix
from callix import NodeKind, RelationKind

MODULE = '''\
def label(r):
    return r


def run(rows):
    a = ", ".join(label(r) for r in rows)
    b = ", ".join([label(r) for r in rows])
    return a, b
'''

CHAINED = '''\
def label(r):
    return r


def make():
    return print


def run(rows):
    make()(label(r) for r in rows)
'''

CALLBACK = '''\
def f(x):
    return x


def run(fs):
    return list(map(f for f in fs))
'''


def _project(root: Path, source: str) -> callix.Graph:
    pkg = root / "pkg"
    pkg.mkdir()
    (pkg / "__init__.py").write_text("")
    (pkg / "m.py").write_text(source)
    return callix.PythonAdapter().analyze(str(root))


def _edges_at(graph: callix.Graph, source: str, line: int) -> Counter[tuple[str, str, str, str | None]]:
    """(kind, source name, target name, resolution) of the edges whose span starts on `line`.

    An external target is named by the identifier read from the source at its span.
    """
    lines = source.split("\n")
    found: Counter[tuple[str, str, str, str | None]] = Counter()
    for r in graph.relations:
        span = r.metadata.get("span")
        if not isinstance(span, callix.Span) or span.start_line != line:
            continue
        if r.kind not in (RelationKind.CALLS, RelationKind.REFERENCES):
            continue
        target = graph.nodes[r.target_id]
        if target.kind is NodeKind.EXTERNAL_SYMBOL:
            text = lines[span.start_line - 1]
            name = text[span.start_col - 1 : span.end_col - 1] if span.end_line == span.start_line else text
        else:
            name = target.qualified_name
        if name in ("pkg.m.run.a", "pkg.m.run.b"):
            # the assignment targets differ between the two lines being compared
            continue
        found[(r.kind.value, graph.nodes[r.source_id].qualified_name, name, r.metadata.get("resolution"))] += 1
    return found


def test_sole_generator_argument_is_scanned(tmp_path: Path) -> None:
    graph = _project(tmp_path, MODULE)
    gen = _edges_at(graph, MODULE, 6)
    lst = _edges_at(graph, MODULE, 7)
    assert ("calls", "pkg.m.run", "pkg.m.label", "definition") in gen
    assert ("references", "pkg.m.run", "pkg.m.run.rows", "definition") in gen
    assert gen == lst


def test_generator_argument_after_a_call_result(tmp_path: Path) -> None:
    graph = _project(tmp_path, CHAINED)
    got = _edges_at(graph, CHAINED, 10)
    assert ("calls", "pkg.m.run", "pkg.m.label", "definition") in got


def test_generator_is_not_a_callback(tmp_path: Path) -> None:
    graph = _project(tmp_path, CALLBACK)
    got = _edges_at(graph, CALLBACK, 6)
    assert not [e for e in got if e[2] == "pkg.m.f"]
