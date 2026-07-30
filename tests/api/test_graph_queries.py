"""Traversal and slicing: `outgoing`/`incoming`, `neighbors`, `subgraph`.

The ordering assertions here are the point of the file. A slice of a graph has
to serialize the same way on every machine and in every process, or it cannot
be a cache key, a diff input or a golden file.
"""

from __future__ import annotations

import pytest

from callix import Graph, NodeKind, RelationKind

from tests.graphs import graph_of, node, relation


# -- edges -------------------------------------------------------------------


def test_outgoing_and_incoming_are_opposite_ends():
    graph = graph_of("ab", [("a", "b")])
    assert [r.target_id for r in graph.outgoing("a")] == ["b"]
    assert graph.outgoing("b") == []
    assert [r.source_id for r in graph.incoming("b")] == ["a"]
    assert graph.incoming("a") == []


def test_edges_of_an_unknown_node_are_empty_not_an_error():
    graph = graph_of("a")
    assert graph.outgoing("nope") == []
    assert graph.incoming("nope") == []


def test_edge_kind_filter():
    graph = Graph()
    for id in "abc":
        graph.add_node(node(id))
    graph.add_relation(relation("a", "b", RelationKind.CALLS))
    graph.add_relation(relation("a", "c", RelationKind.IMPORTS))

    assert [r.target_id for r in graph.outgoing("a")] == ["b", "c"]
    assert [r.target_id for r in graph.outgoing("a", RelationKind.CALLS)] == ["b"]
    assert [r.target_id for r in graph.outgoing("a", RelationKind.IMPORTS)] == ["c"]
    assert graph.outgoing("a", RelationKind.HAS_TYPE) == []


def test_callees_callers_and_references_use_the_right_kinds():
    graph = Graph()
    for id in "abc":
        graph.add_node(node(id))
    graph.add_relation(relation("a", "b", RelationKind.CALLS))
    graph.add_relation(relation("c", "b", RelationKind.REFERENCES))

    assert [n.id for n in graph.callees("a")] == ["b"]
    assert [n.id for n in graph.callers("b")] == ["a"]
    # references_to follows REFERENCES, not CALLS.
    assert [n.id for n in graph.references_to("b")] == ["c"]
    assert graph.callees("b") == []


def test_a_query_after_a_mutation_sees_the_new_edge():
    # The edge indices are cached and must be invalidated on write.
    graph = graph_of("ab")
    assert graph.callees("a") == []
    graph.add_relation(relation("a", "b", RelationKind.CALLS))
    assert [n.id for n in graph.callees("a")] == ["b"]


# -- neighbors ---------------------------------------------------------------


def test_neighbors_walks_outward_and_excludes_the_start():
    graph = graph_of("abcd", [("a", "b"), ("b", "c"), ("c", "d")])
    assert [n.id for n in graph.neighbors("a", 0)] == []
    assert [n.id for n in graph.neighbors("a", 1)] == ["b"]
    assert [n.id for n in graph.neighbors("a", 2)] == ["b", "c"]
    assert [n.id for n in graph.neighbors("a", 3)] == ["b", "c", "d"]
    # Past the end of the chain it simply stops.
    assert [n.id for n in graph.neighbors("a", 99)] == ["b", "c", "d"]


def test_neighbors_of_a_cycle_terminates_and_omits_the_start():
    graph = graph_of("ab", [("a", "b"), ("b", "a")])
    assert [n.id for n in graph.neighbors("a", 5)] == ["b"]


def test_neighbors_of_a_self_edge_is_empty():
    graph = graph_of("a", [("a", "a")])
    assert graph.neighbors("a", 1) == []


def test_neighbors_follows_both_directions():
    graph = graph_of("abc", [("a", "b"), ("c", "b")])
    assert [n.id for n in graph.neighbors("b", 1)] == ["a", "c"]


# -- subgraph ----------------------------------------------------------------


def test_subgraph_keeps_the_parents_node_order_not_the_arguments():
    """The argument order must not leak into the result.

    A `HashSet` here made the node order differ between processes, because Rust
    randomises its iteration; five runs gave five orderings and
    `subgraph().to_dict()` was unusable as a cache key.
    """
    graph = graph_of("abcd")
    assert list(graph.subgraph(["d", "b", "a"]).nodes) == ["a", "b", "d"]
    assert list(graph.subgraph(["a", "b", "d"]).nodes) == ["a", "b", "d"]


def test_subgraph_node_order_is_stable_across_calls():
    graph = graph_of("abcdefgh")
    ids = list(graph.nodes)
    first = list(graph.subgraph(ids).nodes)
    for _ in range(20):
        assert list(graph.subgraph(list(reversed(ids))).nodes) == first


def test_subgraph_ignores_ids_that_are_not_there():
    graph = graph_of("ab")
    assert list(graph.subgraph(["a", "nope"]).nodes) == ["a"]
    assert len(graph.subgraph([]).nodes) == 0


def test_subgraph_pulls_in_the_far_end_of_an_incident_edge():
    # An edge is kept when either end is selected, so the other end has to be
    # brought along or the result would hold a dangling relation.
    graph = graph_of("abc", [("a", "b")])
    sub = graph.subgraph(["a"])
    assert set(sub.nodes) == {"a", "b"}
    assert len(sub.relations) == 1


def test_subgraph_drops_edges_touching_nothing_selected():
    graph = graph_of("abcd", [("a", "b"), ("c", "d")])
    sub = graph.subgraph(["a"])
    assert len(sub.relations) == 1
    assert set(sub.nodes) == {"a", "b"}


def test_subgraph_carries_the_metadata():
    """Otherwise a slice of a resolved graph looks like an unresolved one."""
    graph = graph_of("ab", [("a", "b")])
    graph.metadata["resolver_status"] = "ok"
    graph.metadata["language"] = "python"
    sub = graph.subgraph(["a", "b"])
    assert sub.metadata["resolver_status"] == "ok"
    assert sub.metadata["language"] == "python"


def test_subgraph_for_file_selects_by_file_path():
    graph = Graph()
    graph.add_node(node("a", file_path="x.py"))
    graph.add_node(node("b", file_path="x.py"))
    graph.add_node(node("c", file_path="y.py"))
    graph.add_relation(relation("a", "b"))

    sub = graph.subgraph_for_file("x.py")
    assert list(sub.nodes) == ["a", "b"]
    assert len(sub.relations) == 1
    assert len(graph.subgraph_for_file("nope").nodes) == 0


def test_subgraph_of_a_real_fixture_graph_is_reproducible(python_graph: Graph):
    ids = list(python_graph.nodes)[:12]
    first = python_graph.to_json()
    a = python_graph.subgraph(ids).to_json()
    b = python_graph.subgraph(list(reversed(ids))).to_json()
    assert a == b
    # And slicing must not disturb the parent.
    assert python_graph.to_json() == first
