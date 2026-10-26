"""A lambda is a function nobody named, and the graph says so.

It used to vanish: its body was scanned as part of the scope it was written in,
so what a lambda called was attributed to the enclosing function and a call of
the lambda landed on the variable that held it.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

SOURCE = '''\
def first():
    pass


def second():
    pass


def run(a):
    a()


def make():
    return lambda: second()


handler = lambda x: first()
run(handler)
run(lambda: first())
maker = make()
maker()
'''


def analyse(tmp_path: Path) -> callix.Graph:
    (tmp_path / "app.py").write_text(SOURCE)
    return callix.PythonAdapter().analyze(str(tmp_path))


def calls(graph: callix.Graph) -> set[tuple[str, str, str | None]]:
    out = set()
    for r in graph.relations:
        if r.kind is RelationKind.CALLS:
            out.add(
                (
                    graph.nodes[r.source_id].qualified_name,
                    graph.nodes[r.target_id].qualified_name,
                    r.metadata.get("resolution"),
                )
            )
    return out


def test_a_lambda_is_a_function_named_for_its_scope(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    functions = {n.qualified_name for n in graph.nodes.values() if n.kind is NodeKind.FUNCTION}
    # numbered in the order each scope meets them
    assert {"app.<lambda1>", "app.<lambda2>", "app.make.<lambda1>"} <= functions
    (handler,) = [n for n in graph.nodes.values() if n.qualified_name == "app.<lambda1>"]
    assert handler.metadata["lambda"] is True


def test_a_lambda_has_parameters_and_calls_of_its_own(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    parameters = {n.qualified_name for n in graph.nodes.values() if n.kind is NodeKind.PARAMETER}
    assert "app.<lambda1>.x" in parameters
    found = calls(graph)
    # the body belongs to the lambda, not to the module that wrote it
    assert ("app.<lambda1>", "app.first", "definition") in found
    assert ("app.make.<lambda1>", "app.second", "definition") in found
    assert ("app.py", "app.first", "definition") not in found


def test_a_lambda_is_followed_when_assigned_passed_or_returned(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    found = {(s, t) for s, t, how in calls(graph) if how == "flow"}
    # `run(handler)` and `run(lambda: ...)` reach `a()` in `run`
    assert ("app.run", "app.<lambda1>") in found
    assert ("app.run", "app.<lambda2>") in found
    # `maker = make(); maker()` calls what `make` returns
    assert ("app.py", "app.make.<lambda1>") in found
