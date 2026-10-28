"""A class defined in a function body, and the name span of a qualified record head.

A class, struct or union defined in a function body is a CLASS qualified by the
function (`run()::State`); the function DECLARES it, its members are
`run()::State::stats` and `run()::State::State()`, and it is bound only from
inside that function. `class Outer::Inner {` is the class `Inner`, and its name
span starts on `Inner`, not on the qualifier.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

A_CC = """int run() {
  struct State {
    int stats;
    State() : stats(0) {}
  };
  State state;
  state.stats = 1;
  return state.stats;
}
class Outer { public: class Inner; };
class Outer::Inner {
 public:
  int v;
};
"""

SHADOW_CC = """struct State { int stats; State() : stats(0) {} };
int run() {
  struct State { int stats; State() : stats(0) {} };
  State inner;
  return inner.stats;
}
int other() {
  State outer;
  return outer.stats;
}
"""


def build(root: Path, files: dict[str, str]):
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return callix.CppAdapter().analyze(str(root))


def by_name(graph, qualified_name: str, kind: NodeKind):
    found = [n for n in graph.nodes.values() if n.kind is kind and n.qualified_name == qualified_name]
    assert len(found) == 1, f"{qualified_name}: {found}"
    return found[0]


def edges(graph, source, kind: RelationKind):
    return [r for r in graph.relations if r.source_id == source.id and r.kind is kind]


def at(edge) -> tuple[int, int]:
    span = edge.metadata["span"]
    return (span.start_line, span.start_col)


def test_a_local_class_is_a_class_of_its_function(tmp_path: Path) -> None:
    graph = build(tmp_path, {"a.cc": A_CC})
    run = by_name(graph, "run()", NodeKind.FUNCTION)
    state = by_name(graph, "run()::State", NodeKind.CLASS)
    by_name(graph, "run()::State::stats", NodeKind.ATTRIBUTE)
    constructor = by_name(graph, "run()::State::State()", NodeKind.METHOD)
    assert state.id in [r.target_id for r in edges(graph, run, RelationKind.DECLARES)]
    assert constructor.id in [r.target_id for r in edges(graph, run, RelationKind.CALLS)]


def test_uses_of_a_local_field_bind_to_it(tmp_path: Path) -> None:
    graph = build(tmp_path, {"a.cc": A_CC})
    run = by_name(graph, "run()", NodeKind.FUNCTION)
    stats = by_name(graph, "run()::State::stats", NodeKind.ATTRIBUTE)
    refs = {at(r): r for r in edges(graph, run, RelationKind.REFERENCES) if r.target_id == stats.id}
    assert refs[(7, 9)].metadata["access"] == "write"
    assert refs[(8, 16)].metadata["access"] == "read"
    assert not [n for n in graph.nodes.values() if n.qualified_name.startswith("stats@")]


def test_a_qualified_record_head_names_its_last_component(tmp_path: Path) -> None:
    graph = build(tmp_path, {"a.cc": A_CC})
    inner = by_name(graph, "Outer::Inner", NodeKind.CLASS)
    assert inner.name == "Inner"
    span = inner.metadata["name_span"]
    assert (span.start_line, span.start_col, span.end_line, span.end_col) == (11, 14, 11, 19)


def test_a_global_class_is_not_stolen_by_a_local_one(tmp_path: Path) -> None:
    graph = build(tmp_path, {"a.cc": SHADOW_CC})
    local = by_name(graph, "run()::State", NodeKind.CLASS)
    outer = by_name(graph, "State", NodeKind.CLASS)
    run = by_name(graph, "run()", NodeKind.FUNCTION)
    other = by_name(graph, "other()", NodeKind.FUNCTION)
    run_targets = {r.target_id for r in graph.relations if r.source_id == run.id}
    other_targets = {r.target_id for r in graph.relations if r.source_id == other.id}
    assert local.id in run_targets or by_name(graph, "run()::State::State()", NodeKind.METHOD).id in run_targets
    assert outer.id not in run_targets
    assert local.id not in other_targets
    assert by_name(graph, "State::State()", NodeKind.METHOD).id in other_targets
