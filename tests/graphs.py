"""Graph builders shared by the `tests/unit` and `tests/api` suites.

A separate module rather than `conftest.py` because these are plain
constructors, not fixtures: a test that needs three nodes and an edge should
say so in one line instead of taking a fixture argument.
"""

from __future__ import annotations

import pathlib
from typing import Any

import callix
from callix import Graph, Node, NodeKind, Relation, RelationKind, Span

FIXTURES = pathlib.Path(__file__).resolve().parent / "fixtures"

#: The boundary both fixture projects touch: `tests/fixtures/yaml/openapi.yaml`
#: exposes `GET /engines/{id}` and `tests/fixtures/python/pkg/service.py`
#: consumes it through `httpx.get(f"/engines/{ident}")`. The key is what
#: `normalize_http_path` reduces both spellings to.
SHARED_BOUNDARY_KEY = "GET /engines/{}"
SHARED_BOUNDARY_ID = callix.make_boundary_id("http", SHARED_BOUNDARY_KEY)


def node(
    id: str,
    name: str | None = None,
    kind: NodeKind = NodeKind.FUNCTION,
    *,
    qualified_name: str | None = None,
    **kwargs: Any,
) -> Node:
    """A node with plausible defaults; `id` doubles as the short name."""
    name = name if name is not None else id
    return Node(
        id=id,
        kind=kind,
        qualified_name=qualified_name if qualified_name is not None else f"m.{name}",
        name=name,
        **kwargs,
    )


def relation(
    source_id: str,
    target_id: str,
    kind: RelationKind = RelationKind.CALLS,
    metadata: dict[str, Any] | None = None,
) -> Relation:
    return Relation(source_id, target_id, kind, metadata)


def graph_of(
    node_ids: str | list[str],
    edges: list[tuple[str, str]] | None = None,
    kind: RelationKind = RelationKind.CALLS,
) -> Graph:
    """A graph of single-letter nodes inserted in the given order.

    `graph_of("abc", [("a", "b")])` is three nodes and one CALLS edge. The
    order of `node_ids` *is* the graph's node order, which several tests assert
    against.
    """
    graph = Graph()
    for id in node_ids:
        graph.add_node(node(id))
    for source, target in edges or []:
        graph.add_relation(relation(source, target, kind))
    return graph


def spanned(id: str, file_path: str, span: Span, name_span: Span | None = None) -> Node:
    """A node carrying the spans `SpanIndex.from_graph` reads."""
    metadata = {"name_span": name_span} if name_span is not None else None
    return node(id, file_path=file_path, span=span, metadata=metadata)


def boundary_node(id: str, mechanism: str = "http", key: str = "GET /x") -> Node:
    """A BOUNDARY node shaped the way the adapters emit one."""
    return Node(
        id=id,
        kind=NodeKind.BOUNDARY,
        qualified_name=f"{mechanism}:{key}",
        name=key,
        metadata={"mechanism": mechanism, "key": key},
    )


def boundary_graph(
    consumers: list[tuple[str, float | None]],
    providers: list[tuple[str, float | None]],
    *,
    boundary_id: str = "B",
    mechanism: str = "http",
    key: str = "GET /x",
) -> Graph:
    """One boundary with the given consumer and provider sides.

    Each side is `(node id, confidence)`; a `None` confidence leaves the key
    out of the edge metadata, which is how `link_boundaries` gets exercised on
    the value it falls back to.
    """
    graph = Graph()
    graph.add_node(boundary_node(boundary_id, mechanism, key))
    for id, _ in consumers + providers:
        if id not in graph.nodes:
            graph.add_node(node(id))
    for id, confidence in consumers:
        meta = None if confidence is None else {"confidence": confidence}
        graph.add_relation(relation(id, boundary_id, RelationKind.CONSUMES, meta))
    for id, confidence in providers:
        meta = None if confidence is None else {"confidence": confidence}
        graph.add_relation(relation(id, boundary_id, RelationKind.EXPOSES, meta))
    return graph


def communicates(graph: Graph) -> list[Relation]:
    """Every COMMUNICATES_WITH edge, in insertion order."""
    return [r for r in graph.relations if r.kind == RelationKind.COMMUNICATES_WITH]


def kinds(items) -> list[str]:
    return [item.kind.value for item in items]
