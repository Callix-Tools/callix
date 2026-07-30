"""`NodeKind`, `RelationKind`, `ResolverStatus`.

The `.value` strings are the on-disk format: they appear in every serialized
graph, in `tests/golden`, and in `tests/baseline`. The full tables are spelled
out so that renaming a member — or adding one without deciding on its wire
value — fails here.
"""

from __future__ import annotations

import pytest

from callix import NodeKind, RelationKind, ResolverStatus

NODE_KINDS = {
    NodeKind.PROJECT: "project",
    NodeKind.MODULE: "module",
    NodeKind.FILE: "file",
    NodeKind.CLASS: "class",
    NodeKind.FUNCTION: "function",
    NodeKind.METHOD: "method",
    NodeKind.PARAMETER: "parameter",
    NodeKind.IMPORT: "import",
    NodeKind.DEPENDENCY: "dependency",
    NodeKind.EXTERNAL_SYMBOL: "external_symbol",
    NodeKind.VARIABLE: "variable",
    NodeKind.ATTRIBUTE: "attribute",
    NodeKind.TYPE_ALIAS: "type_alias",
    NodeKind.BOUNDARY: "boundary",
}

RELATION_KINDS = {
    RelationKind.CONTAINS: "contains",
    RelationKind.DECLARES: "declares",
    RelationKind.IMPORTS: "imports",
    RelationKind.CALLS: "calls",
    RelationKind.REFERENCES: "references",
    RelationKind.DEPENDS_ON: "depends_on",
    RelationKind.RESOLVES_TO: "resolves_to",
    RelationKind.INHERITS_FROM: "inherits_from",
    RelationKind.HAS_TYPE: "has_type",
    RelationKind.EXPOSES: "exposes",
    RelationKind.CONSUMES: "consumes",
    RelationKind.COMMUNICATES_WITH: "communicates_with",
}


def test_the_graph_contract_is_fourteen_node_kinds():
    assert len(NODE_KINDS) == 14
    assert len(set(NODE_KINDS.values())) == 14


def test_the_graph_contract_is_twelve_relation_kinds():
    assert len(RELATION_KINDS) == 12
    assert len(set(RELATION_KINDS.values())) == 12


@pytest.mark.parametrize(("kind", "value"), list(NODE_KINDS.items()), ids=NODE_KINDS.values())
def test_node_kind_value_and_round_trip(kind, value):
    assert kind.value == value
    assert NodeKind.from_value(value) is kind
    assert str(kind) == f"NodeKind.{value.upper()}"


@pytest.mark.parametrize(
    ("kind", "value"), list(RELATION_KINDS.items()), ids=RELATION_KINDS.values()
)
def test_relation_kind_value_and_round_trip(kind, value):
    assert kind.value == value
    assert RelationKind.from_value(value) is kind
    assert str(kind) == f"RelationKind.{value.upper()}"


@pytest.mark.parametrize("value", ["", "FUNCTION", "typealias", "external symbol", "nope"])
def test_node_kind_from_value_rejects_an_unknown_string(value):
    # Upper case is the *member* name, not the value: the enum is strict here,
    # unlike ResolverStatus, because a wrong kind is a broken graph.
    with pytest.raises(ValueError, match="unknown NodeKind"):
        NodeKind.from_value(value)


@pytest.mark.parametrize("value", ["", "CALLS", "dependson", "depends-on", "nope"])
def test_relation_kind_from_value_rejects_an_unknown_string(value):
    with pytest.raises(ValueError, match="unknown RelationKind"):
        RelationKind.from_value(value)


def test_kinds_are_comparable_and_hashable():
    assert NodeKind.FUNCTION is NodeKind.from_value("function")
    assert NodeKind.FUNCTION != NodeKind.METHOD
    assert len({NodeKind.FUNCTION, NodeKind.from_value("function")}) == 1
    assert len({RelationKind.CALLS, RelationKind.REFERENCES}) == 2


# -- ResolverStatus --------------------------------------------------------


def test_resolver_status_values():
    assert [s.value for s in (ResolverStatus.OK, ResolverStatus.DEGRADED, ResolverStatus.UNAVAILABLE)] == [
        "ok",
        "degraded",
        "unavailable",
    ]
    assert str(ResolverStatus.DEGRADED) == "ResolverStatus.DEGRADED"


def test_combine_of_nothing_is_ok():
    """An empty list means "no resolver had anything to say", not a failure."""
    assert ResolverStatus.combine([]) is ResolverStatus.OK


@pytest.mark.parametrize(
    ("statuses", "worst"),
    [
        ([ResolverStatus.OK], ResolverStatus.OK),
        ([ResolverStatus.OK, ResolverStatus.OK], ResolverStatus.OK),
        ([ResolverStatus.OK, ResolverStatus.DEGRADED], ResolverStatus.DEGRADED),
        ([ResolverStatus.DEGRADED, ResolverStatus.OK], ResolverStatus.DEGRADED),
        ([ResolverStatus.DEGRADED, ResolverStatus.UNAVAILABLE], ResolverStatus.UNAVAILABLE),
        ([ResolverStatus.UNAVAILABLE, ResolverStatus.OK], ResolverStatus.UNAVAILABLE),
        (
            [ResolverStatus.OK, ResolverStatus.UNAVAILABLE, ResolverStatus.DEGRADED],
            ResolverStatus.UNAVAILABLE,
        ),
    ],
)
def test_combine_picks_the_worst(statuses, worst):
    assert ResolverStatus.combine(statuses) is worst


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("ok", ResolverStatus.OK),
        ("degraded", ResolverStatus.DEGRADED),
        ("unavailable", ResolverStatus.UNAVAILABLE),
        (ResolverStatus.DEGRADED, ResolverStatus.DEGRADED),
        # Anything unrecognized is UNAVAILABLE rather than an error: a foreign
        # or hand-edited graph must stay readable.
        ("nope", ResolverStatus.UNAVAILABLE),
        ("OK", ResolverStatus.UNAVAILABLE),
        ("", ResolverStatus.UNAVAILABLE),
        (None, ResolverStatus.UNAVAILABLE),
        (5, ResolverStatus.UNAVAILABLE),
    ],
)
def test_from_value_is_lenient(value, expected):
    assert ResolverStatus.from_value(value) is expected


def test_from_value_honours_an_explicit_default():
    assert ResolverStatus.from_value("nope", ResolverStatus.OK) is ResolverStatus.OK
    # A recognized value wins over the default.
    assert ResolverStatus.from_value("degraded", ResolverStatus.OK) is ResolverStatus.DEGRADED
