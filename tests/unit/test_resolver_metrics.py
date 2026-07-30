"""`ResolverMetrics` — the counters an adapter leaves in `graph.metadata`.

`as_dict` output is part of the serialized graph (and of the resolved golden
summary), so the key set and the rounding are pinned.
"""

from __future__ import annotations

import pytest

from callix import RESOLVER_METRICS_KEY, ResolverMetrics

FIELDS = ("queries", "resolved", "internal", "external", "unresolved", "seconds")


def test_defaults_are_zero():
    m = ResolverMetrics()
    assert [getattr(m, f) for f in FIELDS] == [0, 0, 0, 0, 0, 0.0]
    assert m.resolved_pct == 0.0


def test_every_field_is_readable_and_writable():
    m = ResolverMetrics(queries=1, resolved=2, internal=3, external=4, unresolved=5, seconds=6.5)
    assert [getattr(m, f) for f in FIELDS] == [1, 2, 3, 4, 5, 6.5]
    for i, field in enumerate(FIELDS):
        setattr(m, field, i + 10)
    assert [getattr(m, f) for f in FIELDS] == [10, 11, 12, 13, 14, 15.0]


def test_resolved_pct_of_no_queries_is_zero_not_a_division_error():
    assert ResolverMetrics(queries=0, resolved=0).resolved_pct == 0.0


@pytest.mark.parametrize(
    ("queries", "resolved", "pct"),
    [(10, 4, 40.0), (4, 4, 100.0), (3, 1, pytest.approx(33.3333, abs=1e-4)), (10, 0, 0.0)],
)
def test_resolved_pct(queries, resolved, pct):
    assert ResolverMetrics(queries=queries, resolved=resolved).resolved_pct == pct


def test_merge_adds_every_counter_in_place():
    total = ResolverMetrics(queries=1, resolved=1, internal=1, external=0, unresolved=0, seconds=0.5)
    total.merge(
        ResolverMetrics(queries=9, resolved=3, internal=2, external=1, unresolved=6, seconds=1.25)
    )
    assert [getattr(total, f) for f in FIELDS] == [10, 4, 3, 1, 6, 1.75]
    assert total.resolved_pct == 40.0


def test_merge_leaves_the_other_side_alone():
    other = ResolverMetrics(queries=5)
    ResolverMetrics(queries=1).merge(other)
    assert other.queries == 5


def test_merge_of_a_fresh_metrics_is_a_no_op():
    m = ResolverMetrics(queries=7, resolved=7, seconds=1.0)
    m.merge(ResolverMetrics())
    assert (m.queries, m.resolved, m.seconds) == (7, 7, 1.0)


def test_as_dict_shape():
    d = ResolverMetrics(
        queries=10, resolved=4, internal=3, external=1, unresolved=6, seconds=1.5
    ).as_dict()
    assert d == {
        "queries": 10,
        "resolved": 4,
        "internal": 3,
        "external": 1,
        "unresolved": 6,
        "seconds": 1.5,
        # Derived, not stored — present so a reader never recomputes it.
        "resolved_pct": 40.0,
    }


def test_as_dict_rounds_seconds_to_three_and_pct_to_one():
    d = ResolverMetrics(queries=3, resolved=1, seconds=1.23456).as_dict()
    assert d["seconds"] == 1.235
    assert d["resolved_pct"] == 33.3


def test_as_dict_rounds_half_to_even_like_python():
    # round_to() uses round_ties_even so a graph dumped by callix matches one
    # dumped by graphlens, where the value went through Python's round().
    assert ResolverMetrics(seconds=0.0625).as_dict()["seconds"] == round(0.0625, 3)
    assert ResolverMetrics(seconds=0.0635).as_dict()["seconds"] == round(0.0635, 3)


def test_repr_lists_the_counters_but_not_the_timing():
    m = ResolverMetrics(queries=1, resolved=2, internal=3, external=4, unresolved=5, seconds=9.0)
    assert repr(m) == (
        "ResolverMetrics(queries=1, resolved=2, internal=3, external=4, unresolved=5)"
    )


def test_the_metadata_key_is_stable():
    assert RESOLVER_METRICS_KEY == "resolver_metrics"
