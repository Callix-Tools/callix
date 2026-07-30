"""`normalize_http_path` and `normalize_pkg_name`.

Cross-language boundary matching is exactly as good as this function: the
Python decorator `@app.get("/engines/{id}")`, the Express route
`/engines/:id`, the Flask rule `/engines/<int:id>` and the concrete URL
`https://svc/engines/7` all have to reduce to one key or the graph never joins
up. Every form below is one an adapter actually produces.
"""

from __future__ import annotations

import pytest

from callix import normalize_http_path, normalize_pkg_name


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        # Plain paths, trailing slash dropped except at the root.
        ("/users", "/users"),
        ("/users/", "/users"),
        ("/", "/"),
        ("//", "/"),
        ("", "/"),
        ("users", "/users"),
        ("  /users  ", "/users"),
        # Parameter styles, one per framework family.
        ("/users/{id}", "/users/{}"),          # FastAPI, Starlette, OpenAPI
        ("/users/{id:int}", "/users/{}"),      # Starlette converter
        ("/users/<int:id>", "/users/{}"),      # Flask
        ("/users/:id", "/users/{}"),           # Express, axum, gin
        ("/users/{}", "/users/{}"),            # already normalized
        ("/users/{id}/posts/{postId}", "/users/{}/posts/{}"),
        # A concrete id has to meet the templated route.
        ("/users/1", "/users/{}"),
        ("/a/1/b/2", "/a/{}/b/{}"),
        # Scheme, host, query and fragment are not part of the contract.
        ("https://api.example.com/v1/users/1?x=2#frag", "/v1/users/{}"),
        ("http://host", "/"),
        ("http://host/", "/"),
        ("/users?x=1", "/users"),
        ("/users#f", "/users"),
        # A colon inside a segment is not an Express parameter.
        ("/v1/users/123:activate", "/v1/users/123:activate"),
        ("/sha256:abc", "/sha256:abc"),
        # Nothing else is invented: a wildcard stays a wildcard.
        ("/users/*", "/users/*"),
    ],
)
def test_normalize_http_path(raw, expected):
    assert normalize_http_path(raw) == expected


def test_normalize_http_path_is_idempotent():
    for raw in ("/users/{id}", "https://h/a/1/", "/users/<int:id>", ""):
        once = normalize_http_path(raw)
        assert normalize_http_path(once) == once


def test_the_four_parameter_styles_agree():
    """The property the boundary index relies on."""
    keys = {
        normalize_http_path(p)
        for p in ("/engines/{id}", "/engines/:id", "/engines/<int:id>", "/engines/7")
    }
    assert keys == {"/engines/{}"}


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("requests", "requests"),
        ("requests>=2.0", "requests"),
        ("requests>=2.0 [security]", "requests"),
        ("requests[security]", "requests"),
        ("My-Pkg", "my_pkg"),
        ("Typing-Extensions==4.0", "typing_extensions"),
        # Scoped npm names keep their shape — the leading @ and the slash are
        # part of the name, not punctuation to normalize away.
        ("@scope/pkg", "@scope/pkg"),
    ],
)
def test_normalize_pkg_name(raw, expected):
    assert normalize_pkg_name(raw) == expected
