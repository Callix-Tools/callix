"""What a loop hands out, and the iteration protocol behind it.

`for x in it` calls `__iter__` and `__next__`, and when the body calls `x` it
calls what the iterable handed out: the elements of a container, what a
generator yields, what an iterator's `__next__` returns.
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


def third():
    pass


def gen():
    yield first


class Counter:
    def __iter__(self):
        return self

    def __next__(self):
        return second


handlers = [third]

for a in gen():
    a()

for b in Counter():
    b()

for c in handlers:
    c()


def counter_loop():
    for i in range(3):
        pass
'''


def analyse(tmp_path: Path) -> callix.Graph:
    (tmp_path / "app.py").write_text(SOURCE)
    return callix.PythonAdapter().analyze(str(tmp_path))


def calls(graph: callix.Graph, how: str | None = None) -> set[tuple[str, str]]:
    return {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.CALLS and (how is None or r.metadata.get("resolution") == how)
    }


def test_a_loop_variable_is_a_node_only_when_the_body_calls_it(tmp_path: Path) -> None:
    # Module-level loop targets are module variables (importable globals), so a, b, c
    # exist as before. Inside a function the policy is unchanged: no node unless called.
    graph = analyse(tmp_path)
    variables = {n.qualified_name for n in graph.nodes.values() if n.kind is NodeKind.VARIABLE}
    assert {"app.a", "app.b", "app.c"} <= variables
    assert "app.counter_loop.i" not in variables
    assert "app.i" not in variables


def test_the_body_calls_what_the_iterable_hands_out(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    flow = calls(graph, "flow")
    assert ("app.py", "app.first") in flow  # yielded by gen()
    assert ("app.py", "app.second") in flow  # returned by Counter.__next__
    # an element of handlers: the type checker already finds this one
    assert ("app.py", "app.third") in calls(graph)


def test_the_loop_calls_the_iteration_protocol(tmp_path: Path) -> None:
    found = calls(analyse(tmp_path), "flow")
    assert ("app.py", "app.Counter.__iter__") in found
    assert ("app.py", "app.Counter.__next__") in found
