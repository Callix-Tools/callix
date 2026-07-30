"""`Node`, `Relation`, `Span`, `BoundaryRef`, `OccurrenceRef`, `ResolvedRef`.

The constructors and `__eq__` are the part of the surface a caller writes
against directly, and `__eq__` is what `diff` and `merge(allow_shared=True)`
are built on, so it is pinned field by field here.
"""

from __future__ import annotations

import pytest

from callix import BoundaryRef, Node, NodeKind, OccurrenceRef, Relation, RelationKind, ResolvedRef, Span

# -- Span ------------------------------------------------------------------


def test_span_fields_are_read_back_verbatim():
    span = Span(2, 3, 4, 5)
    assert (span.start_line, span.start_col, span.end_line, span.end_col) == (2, 3, 4, 5)


def test_span_equality_and_hash():
    assert Span(1, 2, 3, 4) == Span(1, 2, 3, 4)
    assert Span(1, 2, 3, 4) != Span(1, 2, 3, 5)
    assert Span(1, 2, 3, 4) != "not a span"
    # Unlike Node and Relation, Span is hashable and usable as a dict key.
    assert hash(Span(1, 2, 3, 4)) == hash(Span(1, 2, 3, 4))
    assert len({Span(1, 2, 3, 4), Span(1, 2, 3, 4), Span(0, 0, 0, 0)}) == 2


def test_span_repr():
    assert repr(Span(1, 2, 3, 4)) == "Span(1:2-3:4)"


@pytest.mark.parametrize(
    ("line", "col", "inside"),
    [
        (2, 3, True),    # exactly the start
        (4, 5, True),    # exactly the end — both bounds are inclusive
        (3, 0, True),    # a line strictly between, any column
        (3, 999, True),
        (2, 2, False),   # same line as the start, one column short
        (4, 6, False),   # same line as the end, one column past
        (1, 99, False),
        (5, 0, False),
    ],
)
def test_span_contains_is_inclusive_on_both_ends(line, col, inside):
    assert Span(2, 3, 4, 5).contains(line, col) is inside


def test_span_contains_on_a_single_position():
    point = Span(7, 7, 7, 7)
    assert point.contains(7, 7)
    assert not point.contains(7, 8)


# -- Node ------------------------------------------------------------------


def test_node_defaults():
    n = Node(id="i", kind=NodeKind.FUNCTION, qualified_name="m.f", name="f")
    assert n.file_path is None
    assert n.span is None
    assert n.metadata == {}
    assert n.kind is NodeKind.FUNCTION


def test_node_keeps_every_field():
    span = Span(1, 0, 3, 0)
    n = Node(
        id="i",
        kind=NodeKind.METHOD,
        qualified_name="m.C.f",
        name="f",
        file_path="m.py",
        span=span,
        metadata={"origin": "internal"},
    )
    assert (n.id, n.qualified_name, n.name, n.file_path) == ("i", "m.C.f", "f", "m.py")
    assert n.span == span
    assert n.metadata == {"origin": "internal"}


def test_node_accepts_positional_arguments():
    # The documented signature is positional as well as keyword; adapters and
    # user code both rely on it.
    n = Node("i", NodeKind.CLASS, "m.C", "C", "m.py", Span(1, 0, 2, 0), {"k": 1})
    assert n.kind is NodeKind.CLASS
    assert n.metadata == {"k": 1}


def test_node_metadata_dict_is_shared_not_copied():
    """The dict handed in is the dict stored — mutating it mutates the node."""
    metadata = {"a": 1}
    n = Node(id="i", kind=NodeKind.FUNCTION, qualified_name="q", name="n", metadata=metadata)
    metadata["b"] = 2
    assert n.metadata == {"a": 1, "b": 2}


def make_node(**overrides):
    fields = dict(
        id="i",
        kind=NodeKind.FUNCTION,
        qualified_name="m.f",
        name="f",
        file_path="m.py",
        span=Span(1, 0, 2, 0),
        metadata={"k": 1},
    )
    fields.update(overrides)
    return Node(**fields)


def test_node_equality_compares_every_field():
    assert make_node() == make_node()


@pytest.mark.parametrize(
    "override",
    [
        {"id": "other"},
        {"kind": NodeKind.METHOD},
        {"qualified_name": "m.g"},
        {"name": "g"},
        {"file_path": "n.py"},
        {"file_path": None},
        {"span": Span(1, 0, 2, 1)},
        {"span": None},
        {"metadata": {"k": 2}},
        {"metadata": {}},
    ],
)
def test_node_inequality_on_each_field(override):
    assert make_node() != make_node(**override)


def test_node_compared_with_a_foreign_type_is_false_not_an_error():
    assert (make_node() == 5) is False
    assert (make_node() != 5) is True


def test_node_repr_names_the_three_identifying_fields():
    assert repr(make_node()) == 'Node(id="i", kind=function, qualified_name="m.f")'


# -- Relation --------------------------------------------------------------


def test_relation_defaults_and_fields():
    r = Relation("a", "b", RelationKind.CALLS)
    assert (r.source_id, r.target_id) == ("a", "b")
    assert r.kind is RelationKind.CALLS
    assert r.metadata == {}


def test_relation_equality_compares_every_field():
    assert Relation("a", "b", RelationKind.CALLS, {"line": 3}) == Relation(
        "a", "b", RelationKind.CALLS, {"line": 3}
    )


@pytest.mark.parametrize(
    "other",
    [
        ("z", "b", RelationKind.CALLS, {"line": 3}),
        ("a", "z", RelationKind.CALLS, {"line": 3}),
        ("a", "b", RelationKind.REFERENCES, {"line": 3}),
        ("a", "b", RelationKind.CALLS, {"line": 4}),
        ("a", "b", RelationKind.CALLS, None),
    ],
)
def test_relation_inequality_on_each_field(other):
    assert Relation("a", "b", RelationKind.CALLS, {"line": 3}) != Relation(*other)


def test_relation_repr():
    assert repr(Relation("a", "b", RelationKind.CALLS)) == 'Relation("a" -calls-> "b")'


# -- hashability -----------------------------------------------------------
#
# Defining `__eq__` on a pyclass drops the inherited `__hash__`, which left
# both types unhashable; `__hash__` is now explicit on each. The contract these
# tests hold to is the only one that matters — equal values hash equal — and
# both hashes are deliberately partial, because `__eq__` compares a metadata
# dict and a dict cannot be hashed.


@pytest.mark.parametrize(
    ("left", "right"),
    [
        (make_node(), make_node()),
        (
            Relation("a", "b", RelationKind.CALLS, {"line": 3}),
            Relation("a", "b", RelationKind.CALLS, {"line": 3}),
        ),
    ],
)
def test_equal_values_hash_equal(left, right):
    assert left == right
    assert hash(left) == hash(right)
    assert len({left, right}) == 1


def test_node_hash_ignores_everything_but_the_id():
    """A collision is allowed; `==` still separates the two in a set."""
    plain = make_node(metadata={})
    tagged = make_node(metadata={"changed": True})
    assert hash(plain) == hash(tagged)
    assert plain != tagged
    assert len({plain, tagged}) == 2


def test_relation_hash_ignores_only_the_metadata():
    bare = Relation("a", "b", RelationKind.CALLS)
    annotated = Relation("a", "b", RelationKind.CALLS, {"line": 1})
    assert hash(bare) == hash(annotated)
    assert bare != annotated
    assert len({bare, annotated}) == 2
    # The kind and both endpoints do take part.
    assert hash(bare) != hash(Relation("a", "b", RelationKind.REFERENCES))
    assert hash(bare) != hash(Relation("a", "z", RelationKind.CALLS))


def test_nodes_can_be_put_in_a_set():
    """The case that motivated `__hash__`: deduplicating query results."""
    nodes = [make_node(id=id) for id in ("a", "b", "a")]
    assert len(set(nodes)) == 2


# -- the small carrier types ----------------------------------------------


def test_boundary_ref_defaults():
    b = BoundaryRef("http", "server", "GET /x", 3, 4)
    assert (b.mechanism, b.role, b.key, b.line, b.col) == ("http", "server", "GET /x", 3, 4)
    assert b.confidence == 1.0
    assert b.detail == {}
    assert repr(b) == 'BoundaryRef(http server "GET /x" at 3:4)'


def test_boundary_ref_full():
    b = BoundaryRef("queue", "client", "orders", 1, 2, 0.5, {"topic": "orders"})
    assert b.confidence == 0.5
    assert b.detail == {"topic": "orders"}


def test_occurrence_ref():
    o = OccurrenceRef("call", 3, 4, "enclosing", Span(3, 4, 3, 9))
    assert (o.role, o.line, o.col, o.enclosing_id) == ("call", 3, 4, "enclosing")
    assert o.span == Span(3, 4, 3, 9)
    assert repr(o) == 'OccurrenceRef(call at 3:4 in "enclosing")'


def test_resolved_ref():
    r = ResolvedRef("pkg.f", "a.py", 1, 2, "function", "internal")
    assert (r.full_name, r.file_path, r.line, r.col, r.kind, r.origin) == (
        "pkg.f",
        "a.py",
        1,
        2,
        "function",
        "internal",
    )
    assert repr(r) == 'ResolvedRef("pkg.f" at "a.py":1:2, origin=internal)'


def test_resolved_ref_without_a_file_renders_a_dash():
    assert repr(ResolvedRef("x", None, 0, 0, "", "")) == 'ResolvedRef("x" at "-":0:0, origin=)'
