"""`add_node`, `add_relation`, `merge` — the three ways a graph changes.

Node order is part of a graph's serialized output, so most of these assert on
order and not only on membership.
"""

from __future__ import annotations

import pytest

from callix import DuplicateNodeError, Graph, NodeKind, RelationKind

from tests.graphs import graph_of, node, relation


def test_nodes_keep_insertion_order():
    graph = graph_of("cab")
    assert list(graph.nodes) == ["c", "a", "b"]


def test_add_node_rejects_a_repeated_id():
    graph = graph_of("a")
    with pytest.raises(DuplicateNodeError):
        graph.add_node(node("a"))


def test_add_node_rejects_a_repeated_id_even_when_identical():
    # `merge(allow_shared=True)` is the way to combine graphs that share nodes;
    # add_node stays strict so an adapter cannot overwrite its own work.
    graph = graph_of("a")
    with pytest.raises(DuplicateNodeError):
        graph.add_node(node("a"))


def test_relations_may_dangle():
    # Nothing validates endpoints: an adapter emits edges while the graph is
    # still being built, and EXTERNAL_SYMBOL targets are created lazily.
    graph = Graph()
    graph.add_relation(relation("nope", "also-nope"))
    assert len(graph.relations) == 1
    assert graph.outgoing("nope") == graph.relations


def test_len_counts_nodes_not_relations():
    graph = graph_of("abc", [("a", "b"), ("b", "c")])
    assert len(graph) == 3


# -- merge -------------------------------------------------------------------


def test_merge_brings_nodes_and_relations():
    target = graph_of("ab", [("a", "b")])
    source = graph_of("cd", [("c", "d")])
    target.merge(source)
    assert list(target.nodes) == ["a", "b", "c", "d"]
    assert len(target.relations) == 2


def test_merge_rejects_a_collision_by_default():
    target = graph_of("ab")
    with pytest.raises(DuplicateNodeError):
        target.merge(graph_of("bc"))


def test_merge_allows_identical_nodes_to_coincide():
    # This is how cross-language graphs combine: two adapters deliberately
    # produce the same BOUNDARY node.
    target = graph_of("ab")
    target.merge(graph_of("bc"), allow_shared=True)
    assert list(target.nodes) == ["a", "b", "c"]


def test_merge_still_rejects_different_nodes_sharing_an_id():
    target = Graph()
    target.add_node(node("x", "one"))
    source = Graph()
    source.add_node(node("x", "two"))
    with pytest.raises(DuplicateNodeError):
        target.merge(source, allow_shared=True)


def test_a_failed_merge_leaves_the_target_untouched():
    """Either the whole graph merges or none of it does.

    The colliding node is deliberately LAST in the source, because that is the
    ordering under which a validate-as-you-go merge corrupts the target: the
    nodes before it are already in.
    """
    target = Graph()
    target.add_node(node("b"))

    source = Graph()
    source.add_node(node("c"))
    source.add_node(node("b", "different"))
    source.add_relation(relation("c", "b"))

    with pytest.raises(DuplicateNodeError):
        target.merge(source, allow_shared=True)

    assert list(target.nodes) == ["b"]
    assert target.relations == []


def test_merge_appends_relations_without_deduplicating():
    """Merging the same graph twice doubles its edges — call it once per source.

    Deliberate, and the reason `merge` is documented as append-only: an edge
    carrying metadata records where it was observed, and two sources reporting
    the same call site are two observations.
    """
    target = graph_of("ab", [("a", "b")])
    target.merge(graph_of("ab", [("a", "b")]), allow_shared=True)
    assert len(target.nodes) == 2
    assert len(target.relations) == 2


def test_merge_takes_the_worst_resolver_status():
    # Otherwise a degraded side is masked by whichever graph merged last.
    target = graph_of("a")
    target.metadata["resolver_status"] = "ok"
    source = graph_of("b")
    source.metadata["resolver_status"] = "degraded"
    target.merge(source)
    assert target.metadata["resolver_status"] == "degraded"


def test_merge_worst_status_is_order_independent():
    target = graph_of("a")
    target.metadata["resolver_status"] = "degraded"
    source = graph_of("b")
    source.metadata["resolver_status"] = "ok"
    target.merge(source)
    assert target.metadata["resolver_status"] == "degraded"


def test_merged_metadata_carries_other_keys_over():
    target = graph_of("a")
    source = graph_of("b")
    source.metadata["language"] = "yaml"
    target.merge(source)
    assert target.metadata["language"] == "yaml"


def test_metadata_is_a_live_reference():
    # Documented behaviour rather than a copy: adapters write status and
    # metrics into it after the graph is built.
    graph = Graph()
    graph.metadata["k"] = 1
    assert graph.metadata["k"] == 1


def test_kind_filters_are_honoured_after_a_merge():
    target = graph_of("ab", [("a", "b")], RelationKind.CALLS)
    source = graph_of("cd", [("c", "d")], RelationKind.IMPORTS)
    target.merge(source)
    assert len(target.outgoing("a", RelationKind.CALLS)) == 1
    assert target.outgoing("a", RelationKind.IMPORTS) == []
    assert len(target.outgoing("c", RelationKind.IMPORTS)) == 1


def test_nodes_by_kind_and_name_and_file():
    graph = Graph()
    graph.add_node(node("a", "alpha", NodeKind.CLASS, file_path="x.py"))
    graph.add_node(node("b", "beta", NodeKind.FUNCTION, file_path="x.py"))
    graph.add_node(node("c", "gamma", NodeKind.FUNCTION, file_path="y.py"))

    assert [n.id for n in graph.nodes_by_kind(NodeKind.FUNCTION)] == ["b", "c"]
    assert [n.id for n in graph.nodes_by_kind(NodeKind.CLASS)] == ["a"]
    assert [n.id for n in graph.nodes_in_file("x.py")] == ["a", "b"]
    assert graph.nodes_in_file("nope") == []
    # Matches the short name or the qualified one.
    assert [n.id for n in graph.nodes_by_name("beta")] == ["b"]
    assert [n.id for n in graph.nodes_by_name("m.beta")] == ["b"]
