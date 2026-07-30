"""Serialization round-trips and `diff`.

`to_dict`/`from_dict` and `to_json`/`from_json` are the format 1.0 freezes, so
what matters is that nothing is lost — including node ORDER, which is part of
the output — and that malformed input fails with callix's own exception types
rather than a panic or a bare ValueError.
"""

from __future__ import annotations

import json

import pytest

from callix import (
    Graph,
    NodeKind,
    RelationKind,
    SCHEMA_VERSION,
    SerializationError,
    Span,
)

from tests.graphs import graph_of, node, relation


def _rich_graph() -> Graph:
    graph = Graph()
    graph.add_node(
        node(
            "a",
            "alpha",
            NodeKind.CLASS,
            file_path="pkg/x.py",
            span=Span(1, 1, 9, 2),
            metadata={"origin": "internal", "n": 3},
        )
    )
    graph.add_node(node("b", "beta", NodeKind.METHOD, file_path="pkg/x.py"))
    graph.add_relation(relation("a", "b", RelationKind.DECLARES))
    graph.add_relation(relation("b", "a", RelationKind.CALLS, {"line": 4, "col": 9}))
    graph.metadata["resolver_status"] = "ok"
    graph.metadata["resolver_metrics"] = {"queries": 2, "resolved": 1}
    return graph


# -- round trips -------------------------------------------------------------


def test_json_round_trip_is_lossless():
    original = _rich_graph()
    restored = Graph.from_json(original.to_json())

    assert list(restored.nodes) == list(original.nodes)
    assert original.diff(restored).is_empty
    assert restored.to_json() == original.to_json()


def test_dict_round_trip_is_lossless():
    original = _rich_graph()
    restored = Graph.from_dict(original.to_dict())
    assert restored.to_json() == original.to_json()


def test_round_trip_keeps_graph_metadata():
    restored = Graph.from_json(_rich_graph().to_json())
    assert restored.metadata["resolver_status"] == "ok"
    assert restored.metadata["resolver_metrics"] == {"queries": 2, "resolved": 1}


def test_round_trip_keeps_node_detail():
    restored = Graph.from_json(_rich_graph().to_json())
    a = restored.nodes["a"]
    assert (a.kind, a.name, a.qualified_name) == (NodeKind.CLASS, "alpha", "m.alpha")
    assert a.file_path == "pkg/x.py"
    assert (a.span.start_line, a.span.end_col) == (1, 2)
    assert a.metadata["origin"] == "internal"


def test_round_trip_keeps_relation_metadata():
    restored = Graph.from_json(_rich_graph().to_json())
    calls = [r for r in restored.relations if r.kind == RelationKind.CALLS]
    assert [dict(r.metadata) for r in calls] == [{"line": 4, "col": 9}]


def test_round_trip_preserves_a_surprising_node_order():
    graph = graph_of("dcba")
    assert list(Graph.from_json(graph.to_json()).nodes) == ["d", "c", "b", "a"]


def test_to_json_indent_changes_only_the_formatting():
    graph = _rich_graph()
    flat, pretty = graph.to_json(), graph.to_json(indent=2)
    assert flat != pretty
    assert "\n" in pretty
    assert json.loads(flat) == json.loads(pretty)


def test_the_payload_declares_its_schema_version():
    assert _rich_graph().to_dict()["schema_version"] == SCHEMA_VERSION


def test_a_fixture_graph_round_trips(python_graph: Graph):
    restored = Graph.from_json(python_graph.to_json())
    assert list(restored.nodes) == list(python_graph.nodes)
    assert len(restored.relations) == len(python_graph.relations)
    assert python_graph.diff(restored).is_empty


# -- malformed input ---------------------------------------------------------


@pytest.mark.parametrize(
    "text",
    [
        "",
        "not json",
        "[]",
        "null",
        '{"nodes": []}',
    ],
    ids=["empty", "garbage", "list", "null", "no-version"],
)
def test_from_json_rejects_malformed_payloads(text: str):
    with pytest.raises(SerializationError):
        Graph.from_json(text)


def test_from_dict_rejects_a_future_schema_version():
    payload = _rich_graph().to_dict()
    payload["schema_version"] = SCHEMA_VERSION + 1
    with pytest.raises(SerializationError):
        Graph.from_dict(payload)


def test_from_dict_rejects_a_node_that_is_missing_a_field():
    payload = _rich_graph().to_dict()
    del payload["nodes"][0]["qualified_name"]
    with pytest.raises(SerializationError):
        Graph.from_dict(payload)


def test_from_dict_rejects_an_unknown_kind():
    payload = _rich_graph().to_dict()
    payload["nodes"][0]["kind"] = "wormhole"
    with pytest.raises(SerializationError):
        Graph.from_dict(payload)


def test_a_relation_pointing_at_nothing_survives_deserialization():
    # Dangling edges are legal in a graph, so they must be legal in a payload:
    # a subgraph written out and read back would otherwise fail to load.
    payload = _rich_graph().to_dict()
    payload["relations"].append(
        {"source_id": "a", "target_id": "ghost", "kind": "calls", "metadata": {}}
    )
    restored = Graph.from_dict(payload)
    assert any(r.target_id == "ghost" for r in restored.relations)


# -- diff --------------------------------------------------------------------


def test_diff_of_a_graph_with_itself_is_empty():
    graph = _rich_graph()
    diff = graph.diff(graph)
    assert diff.is_empty
    assert (diff.added_nodes, diff.removed_nodes, diff.changed_nodes) == ([], [], [])
    assert (diff.added_relations, diff.removed_relations) == ([], [])


def test_diff_reports_an_added_node():
    diff = graph_of("a").diff(graph_of("ab"))
    assert [n.id for n in diff.added_nodes] == ["b"]
    assert not diff.is_empty


def test_diff_reports_a_removed_node():
    diff = graph_of("ab").diff(graph_of("a"))
    assert [n.id for n in diff.removed_nodes] == ["b"]


def test_diff_reports_a_changed_node_as_old_then_new():
    before = Graph()
    before.add_node(node("a", "one"))
    after = Graph()
    after.add_node(node("a", "two"))

    (old, new) = before.diff(after).changed_nodes[0]
    assert (old.name, new.name) == ("one", "two")


def test_diff_reports_added_and_removed_relations():
    before = graph_of("ab")
    after = graph_of("ab", [("a", "b")])
    assert len(before.diff(after).added_relations) == 1
    assert len(after.diff(before).removed_relations) == 1


def test_diff_notices_relation_metadata():
    before = graph_of("ab")
    before.add_relation(relation("a", "b", RelationKind.CALLS, {"line": 1}))
    after = graph_of("ab")
    after.add_relation(relation("a", "b", RelationKind.CALLS, {"line": 2}))
    diff = before.diff(after)
    assert len(diff.added_relations) == 1
    assert len(diff.removed_relations) == 1


def test_diff_does_not_look_at_graph_metadata():
    """A documented limitation, not an oversight.

    `GraphDiff` describes nodes and relations. A caller comparing resolver
    status or metrics has to read `graph.metadata` — and in particular
    `diff(...).is_empty` is not on its own proof that a round trip was lossless.
    The round-trip tests above therefore compare `to_json()` as well.
    """
    before, after = graph_of("a"), graph_of("a")
    after.metadata["resolver_status"] = "degraded"
    assert before.diff(after).is_empty
