"""`link_boundaries` — the step that makes callix cross-language.

Two adapters produce the same BOUNDARY node because a boundary id depends only
on the mechanism and the normalized key. This is the pass that turns "both
sides touch one node" into an edge between the sides themselves.
"""

from __future__ import annotations

import pytest

from callix import Graph, NodeKind, RelationKind

from tests.graphs import (
    SHARED_BOUNDARY_KEY,
    boundary_graph,
    communicates,
    graph_of,
)


def test_a_consumer_and_a_provider_are_linked():
    graph = boundary_graph(consumers=[("client", 1.0)], providers=[("server", 1.0)])
    assert graph.link_boundaries() == 1
    (edge,) = communicates(graph)
    # The direction is consumer -> provider: the caller depends on the callee.
    assert (edge.source_id, edge.target_id) == ("client", "server")
    assert edge.kind == RelationKind.COMMUNICATES_WITH


def test_the_edge_records_which_boundary_it_came_through():
    graph = boundary_graph(consumers=[("client", 1.0)], providers=[("server", 1.0)])
    graph.link_boundaries()
    (edge,) = communicates(graph)
    assert edge.metadata["mechanism"] == "http"
    assert edge.metadata["boundary_key"] == "GET /x"
    assert edge.metadata["boundary_id"] == "B"


def test_confidence_is_the_product_of_the_two_sides():
    graph = boundary_graph(consumers=[("client", 0.9)], providers=[("server", 0.5)])
    graph.link_boundaries()
    (edge,) = communicates(graph)
    assert edge.metadata["confidence"] == pytest.approx(0.45)


def test_a_missing_confidence_counts_as_certain():
    graph = boundary_graph(consumers=[("client", None)], providers=[("server", None)])
    graph.link_boundaries()
    (edge,) = communicates(graph)
    assert edge.metadata["confidence"] == pytest.approx(1.0)


def test_min_confidence_is_applied_to_the_product():
    # 0.9 * 0.5 = 0.45, so a cutoff of 0.5 excludes it even though each side
    # on its own clears the bar.
    graph = boundary_graph(consumers=[("client", 0.9)], providers=[("server", 0.5)])
    assert graph.link_boundaries(min_confidence=0.5) == 0
    assert communicates(graph) == []


def test_min_confidence_is_inclusive_at_the_boundary():
    graph = boundary_graph(consumers=[("client", 0.5)], providers=[("server", 1.0)])
    assert graph.link_boundaries(min_confidence=0.5) == 1


def test_a_one_sided_boundary_links_nothing():
    assert boundary_graph([("client", 1.0)], []).link_boundaries() == 0
    assert boundary_graph([], [("server", 1.0)]).link_boundaries() == 0


def test_every_pair_is_linked():
    graph = boundary_graph(
        consumers=[("c1", 1.0), ("c2", 1.0)],
        providers=[("s1", 1.0), ("s2", 1.0)],
    )
    assert graph.link_boundaries() == 4
    pairs = {(e.source_id, e.target_id) for e in communicates(graph)}
    assert pairs == {("c1", "s1"), ("c1", "s2"), ("c2", "s1"), ("c2", "s2")}


def test_calling_it_twice_adds_nothing():
    """Idempotent, so a caller may link after every merge without doubling up."""
    graph = boundary_graph([("client", 1.0)], [("server", 1.0)])
    assert graph.link_boundaries() == 1
    assert graph.link_boundaries() == 0
    assert len(communicates(graph)) == 1


def test_a_node_on_both_sides_is_not_linked_to_itself():
    # A service that exposes an endpoint and also calls it is talking to
    # itself, which is not communication between components.
    graph = boundary_graph([("svc", 1.0)], [("svc", 1.0)])
    assert graph.link_boundaries() == 0
    assert communicates(graph) == []


def test_a_shared_boundary_still_links_the_other_pairs():
    # `svc` is skipped against itself but not against the other provider.
    graph = boundary_graph([("svc", 1.0)], [("svc", 1.0), ("other", 1.0)])
    assert graph.link_boundaries() == 1
    assert [(e.source_id, e.target_id) for e in communicates(graph)] == [
        ("svc", "other")
    ]


def test_a_graph_with_no_boundaries_is_a_no_op():
    graph = graph_of("ab", [("a", "b")])
    assert graph.link_boundaries() == 0


def test_boundaries_of_different_mechanisms_do_not_meet():
    graph = Graph()
    http = boundary_graph([("c", 1.0)], [], boundary_id="H", mechanism="http")
    queue = boundary_graph([], [("s", 1.0)], boundary_id="Q", mechanism="queue")
    graph.merge(http)
    graph.merge(queue, allow_shared=True)
    assert graph.link_boundaries() == 0


# -- the real thing ----------------------------------------------------------


def test_python_client_meets_an_openapi_server(cross_language_graph: Graph):
    """The headline capability, on the actual fixtures.

    `tests/fixtures/yaml/openapi.yaml` declares `GET /engines/{id}` and
    `tests/fixtures/python/pkg/service.py` calls `httpx.get(f"/engines/{ident}")`.
    Analysed separately and merged, they must meet in ONE boundary node.
    """
    graph = cross_language_graph
    shared = [
        n
        for n in graph.nodes.values()
        if n.kind == NodeKind.BOUNDARY and n.metadata.get("key") == SHARED_BOUNDARY_KEY
    ]
    assert len(shared) == 1, "the two sides did not collapse into one node"

    sides = {r.kind.value for r in graph.incoming(shared[0].id)}
    assert sides == {"exposes", "consumes"}

    assert graph.link_boundaries(min_confidence=0.5) == 1
    (edge,) = communicates(graph)
    assert graph.nodes[edge.source_id].qualified_name == "pkg/service.py"
    assert graph.nodes[edge.target_id].qualified_name == "openapi.yaml"
    # The client side is inferred from a call, so it is below certainty.
    assert 0.0 < edge.metadata["confidence"] < 1.0


def test_the_shared_boundary_id_is_reproducible_from_the_key_alone():
    """Why the two sides meet at all: the id ignores everything language-specific."""
    from callix import make_boundary_id

    from tests.graphs import SHARED_BOUNDARY_ID

    assert make_boundary_id("http", SHARED_BOUNDARY_KEY) == SHARED_BOUNDARY_ID
    assert make_boundary_id("http", SHARED_BOUNDARY_KEY) != make_boundary_id(
        "queue", SHARED_BOUNDARY_KEY
    )
