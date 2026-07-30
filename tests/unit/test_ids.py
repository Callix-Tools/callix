"""Node and boundary IDs.

These IDs end up in golden files, in `tests/baseline`, and in whatever the
caller stored last week, so the *formula* is frozen here against an
independently computed sha256 rather than against callix's own output. A change
in the recipe cannot pass this file quietly.
"""

from __future__ import annotations

import hashlib

import pytest

from callix import make_boundary_id, make_node_id


def sha16(key: str) -> str:
    return hashlib.sha256(key.encode("utf-8")).hexdigest()[:16]


@pytest.mark.parametrize(
    ("project", "qualified_name", "kind"),
    [
        ("fixture", "pkg.models.Engine", "class"),
        ("fixture", "pkg.service.fetch_one", "function"),
        ("", "", ""),
        ("проект", "модуль.функция", "function"),
        ("a::b", "c::d", "module"),
    ],
)
def test_node_id_is_sha256_of_project_kind_qname(project, qualified_name, kind):
    expected = sha16(f"{project}::{kind}::{qualified_name}")
    assert make_node_id(project, qualified_name, kind) == expected
    assert len(expected) == 16


def test_node_id_exact_values():
    # Hand-computed, so a refactor of digest16 cannot drift unnoticed.
    assert make_node_id("proj", "pkg.mod.fn", "function") == "cdae80a35d44fe1e"
    assert make_node_id("", "", "") == "d00d9be38948fb14"


@pytest.mark.parametrize(
    ("mechanism", "key"),
    [("http", "GET /engines/{}"), ("grpc", "svc.Method"), ("queue", "orders"), ("", "")],
)
def test_boundary_id_is_sha256_of_mechanism_and_key(mechanism, key):
    assert make_boundary_id(mechanism, key) == sha16(f"boundary::{mechanism}::{key}")


def test_boundary_id_exact_value():
    assert make_boundary_id("http", "GET /x") == "621cdb08d8731d98"


def test_boundary_id_ignores_project_and_language():
    """The whole point: two adapters must land on the same node."""
    assert make_boundary_id("http", "GET /users/{}") == make_boundary_id(
        "http", "GET /users/{}"
    )


def test_ids_are_hex_of_the_first_eight_bytes():
    id = make_node_id("p", "q", "function")
    assert len(id) == 16
    assert int(id, 16) == int.from_bytes(hashlib.sha256(b"p::function::q").digest()[:8])


@pytest.mark.parametrize(
    ("a", "b"),
    [
        (("p", "q", "function"), ("p", "q", "method")),
        (("p", "q", "function"), ("p", "r", "function")),
        (("p", "q", "function"), ("o", "q", "function")),
    ],
)
def test_every_component_changes_the_node_id(a, b):
    assert make_node_id(*a) != make_node_id(*b)


def test_the_two_id_namespaces_share_one_formula():
    """Both IDs are `sha256("{a}::{b}::{c}")[:16]` over three fields.

    So `make_boundary_id(m, k)` is literally `make_node_id("boundary", k, m)`.
    Nothing separates the namespaces beyond the fact that a node's third field
    is always a `NodeKind` value and never a mechanism name — a project called
    `boundary` cannot collide with a boundary because no NodeKind is spelled
    `http`, `grpc`, `queue` or `temporal`. Recorded so that a future mechanism
    named after a NodeKind is recognized as the collision it would be.
    """
    assert make_boundary_id("http", "GET /x") == make_node_id("boundary", "GET /x", "http")
