"""`node_to_dict` / `node_from_dict`, `relation_*`, `ensure_schema_version`.

The dict shape *is* the on-disk format: key order included, because a graph is
compared byte for byte against `tests/golden` and against graphlens's output.
Metadata is an open dict, and `Span` is the one type that survives it through a
`{"__span__": [...]}` tag.
"""

from __future__ import annotations

import json

import pytest

from callix import (
    SCHEMA_VERSION,
    NodeKind,
    RelationKind,
    SerializationError,
    Span,
    ensure_schema_version,
    node_from_dict,
    node_to_dict,
    relation_from_dict,
    relation_to_dict,
)

from tests.graphs import node, relation


def test_schema_version_is_one():
    assert SCHEMA_VERSION == 1
    assert isinstance(SCHEMA_VERSION, int)


def test_node_to_dict_key_order_and_shape():
    n = node("i", "f", file_path="a.py", span=Span(1, 0, 3, 4), metadata={"origin": "stdlib"})
    d = node_to_dict(n)
    assert list(d) == ["id", "kind", "qualified_name", "name", "file_path", "span", "metadata"]
    assert d == {
        "id": "i",
        "kind": "function",          # the wire value, not the member name
        "qualified_name": "m.f",
        "name": "f",
        "file_path": "a.py",
        "span": [1, 0, 3, 4],        # a flat list, not an object
        "metadata": {"origin": "stdlib"},
    }


def test_node_to_dict_keeps_the_empty_fields_explicit():
    """`None` is written out rather than omitted, so every node has 7 keys."""
    d = node_to_dict(node("i"))
    assert d["file_path"] is None
    assert d["span"] is None
    assert d["metadata"] == {}


def test_relation_to_dict_key_order_and_shape():
    d = relation_to_dict(relation("a", "b", RelationKind.HAS_TYPE, {"line": 3}))
    assert list(d) == ["source_id", "target_id", "kind", "metadata"]
    assert d == {
        "source_id": "a",
        "target_id": "b",
        "kind": "has_type",
        "metadata": {"line": 3},
    }


def test_node_round_trip():
    n = node("i", "f", NodeKind.METHOD, file_path="a.py", span=Span(2, 1, 9, 0),
             metadata={"origin": "internal"})
    assert node_from_dict(node_to_dict(n)) == n


def test_relation_round_trip():
    r = relation("a", "b", RelationKind.INHERITS_FROM, {"k": [1, 2]})
    assert relation_from_dict(relation_to_dict(r)) == r


def test_dicts_are_json_serializable():
    n = node("i", file_path="a.py", span=Span(1, 0, 2, 0), metadata={"s": Span(1, 1, 1, 2)})
    assert json.loads(json.dumps(node_to_dict(n))) == node_to_dict(n)


# -- metadata encoding ----------------------------------------------------


def test_metadata_span_survives_the_round_trip_through_a_tag():
    n = node("i", metadata={"name_span": Span(4, 8, 4, 12)})
    d = node_to_dict(n)
    assert d["metadata"] == {"name_span": {"__span__": [4, 8, 4, 12]}}
    assert node_from_dict(d).metadata == {"name_span": Span(4, 8, 4, 12)}


def test_metadata_span_survives_nested_inside_containers():
    n = node("i", metadata={"outer": {"inner": Span(1, 1, 1, 2)}, "list": [Span(2, 2, 2, 3)]})
    restored = node_from_dict(node_to_dict(n))
    assert restored.metadata == {
        "outer": {"inner": Span(1, 1, 1, 2)},
        "list": [Span(2, 2, 2, 3)],
    }


@pytest.mark.parametrize(
    "value", [1, 1.5, "s", True, False, None, [1, "a"], {"k": [1, {"j": None}]}]
)
def test_json_native_metadata_is_left_alone(value):
    n = node("i", metadata={"v": value})
    assert node_to_dict(n)["metadata"]["v"] == value
    assert node_from_dict(node_to_dict(n)).metadata == {"v": value}


def test_a_tuple_becomes_a_list_because_json_has_no_tuples():
    assert node_to_dict(node("i", metadata={"v": (1, 2)}))["metadata"]["v"] == [1, 2]


def test_an_unsupported_value_becomes_its_str():
    """Metadata must never make a graph unserializable."""
    class Opaque:
        def __str__(self) -> str:
            return "opaque!"

    assert node_to_dict(node("i", metadata={"v": Opaque()}))["metadata"]["v"] == "opaque!"


def test_non_string_metadata_keys_are_stringified():
    assert node_to_dict(node("i", metadata={7: "x"}))["metadata"] == {"7": "x"}


def test_a_span_shaped_list_is_not_mistaken_for_a_span():
    """Only the `__span__` tag decodes back into a Span."""
    assert node_from_dict(
        {"id": "i", "kind": "function", "qualified_name": "q", "name": "n",
         "metadata": {"v": [1, 2, 3, 4]}}
    ).metadata == {"v": [1, 2, 3, 4]}


@pytest.mark.parametrize(
    "tagged",
    [
        {"__span__": [1, 2, 3]},              # too short
        {"__span__": [1, 2, 3, 4, 5]},        # too long
        {"__span__": [1, 2, 3, "x"]},         # not an int
        {"__span__": [True, 2, 3, 4]},        # bool is an int subclass, but not a coordinate
        {"__span__": [1, 2, 3, 4], "x": 1},   # not the only key
    ],
)
def test_a_malformed_span_tag_stays_a_plain_dict(tagged):
    restored = node_from_dict(
        {"id": "i", "kind": "function", "qualified_name": "q", "name": "n",
         "metadata": {"v": tagged}}
    )
    assert restored.metadata == {"v": tagged}


# -- from_dict input handling ---------------------------------------------


@pytest.mark.parametrize("missing", ["id", "kind", "qualified_name", "name"])
def test_node_from_dict_requires_the_four_identifying_keys(missing):
    data = {"id": "i", "kind": "function", "qualified_name": "q", "name": "n"}
    del data[missing]
    # SerializationError, not KeyError: reading a payload has one failure type.
    with pytest.raises(SerializationError, match=missing):
        node_from_dict(data)


def test_node_from_dict_tolerates_missing_optional_keys():
    n = node_from_dict({"id": "i", "kind": "function", "qualified_name": "q", "name": "n"})
    assert (n.file_path, n.span, n.metadata) == (None, None, {})


def test_node_from_dict_rejects_an_unknown_kind():
    with pytest.raises(SerializationError, match="unknown NodeKind"):
        node_from_dict({"id": "i", "kind": "nope", "qualified_name": "q", "name": "n"})


@pytest.mark.parametrize("missing", ["source_id", "target_id", "kind"])
def test_relation_from_dict_requires_every_key(missing):
    data = {"source_id": "a", "target_id": "b", "kind": "calls"}
    del data[missing]
    with pytest.raises(SerializationError, match=missing):
        relation_from_dict(data)


def test_relation_from_dict_rejects_an_unknown_kind():
    with pytest.raises(SerializationError, match="unknown RelationKind"):
        relation_from_dict({"source_id": "a", "target_id": "b", "kind": "nope"})


def test_a_garbage_span_is_dropped_rather_than_raising():
    for span in ("nope", [1, 2], [1, 2, 3, "x"], {}, 5):
        n = node_from_dict(
            {"id": "i", "kind": "function", "qualified_name": "q", "name": "n", "span": span}
        )
        assert n.span is None, span


def test_garbage_metadata_becomes_an_empty_dict():
    for metadata in ("nope", 5, [1], None):
        n = node_from_dict(
            {"id": "i", "kind": "function", "qualified_name": "q", "name": "n",
             "metadata": metadata}
        )
        assert n.metadata == {}, metadata


# -- ensure_schema_version ------------------------------------------------


def test_ensure_schema_version_accepts_the_current_version():
    assert ensure_schema_version({"schema_version": SCHEMA_VERSION}) is None


@pytest.mark.parametrize(
    "data",
    [
        {},                            # missing entirely
        {"schema_version": None},
        {"schema_version": 0},
        {"schema_version": 2},
        {"schema_version": "1"},       # the string form is not accepted
        {"schema_version": 1.0},
        {"schema_version": -1},
    ],
)
def test_ensure_schema_version_rejects_everything_else(data):
    with pytest.raises(SerializationError, match="Unsupported graph schema_version"):
        ensure_schema_version(data)
