"""The five `extract_*_boundaries` functions.

Each takes source bytes and returns ports — one side of a cross-language
contract. What matters here is that all five agree on the *key*, because the
key alone (with the mechanism) decides which BOUNDARY node a port joins.
"""

from __future__ import annotations

import pytest

from callix import (
    extract_go_boundaries,
    extract_python_boundaries,
    extract_rust_boundaries,
    extract_typescript_boundaries,
    extract_yaml_boundaries,
)

EXTRACTORS = {
    "python": extract_python_boundaries,
    "typescript": extract_typescript_boundaries,
    "go": extract_go_boundaries,
    "rust": extract_rust_boundaries,
    "yaml": extract_yaml_boundaries,
}

PYTHON = b"""
import httpx
from fastapi import FastAPI

app = FastAPI()

@app.get("/engines/{id}")
def read_engine(id: int) -> str:
    return "x"

def push(payload: str):
    return httpx.post("/ingest", json=payload)
"""

TYPESCRIPT = b"""
app.get("/ingest", handler);
const response = await fetch("/engines/1");
"""

GO = b"""
package main

import "net/http"

func main() {
    http.HandleFunc("/engines/{id}", handler)
    http.Get("https://api.example.com/ingest")
}
"""

RUST = b"""
use axum::{routing::get, Router};

fn app() -> Router {
    Router::new().route("/engines/:id", get(handler))
}
"""

YAML = b"""
openapi: 3.0.3
paths:
  /engines/{id}:
    get:
      operationId: readEngine
"""


@pytest.mark.parametrize("name", sorted(EXTRACTORS))
def test_an_empty_source_yields_no_ports(name):
    assert EXTRACTORS[name](b"") == []


@pytest.mark.parametrize("name", sorted(EXTRACTORS))
def test_source_without_boundaries_yields_no_ports(name):
    # Valid-ish in no language in particular; the point is that an extractor
    # invents nothing.
    assert EXTRACTORS[name](b"x\n") == []


def test_python_server_and_client():
    ports = extract_python_boundaries(PYTHON)
    by_role = {p.role: p for p in ports}
    assert set(by_role) == {"server", "client"}

    server = by_role["server"]
    assert (server.mechanism, server.key) == ("http", "GET /engines/{}")
    # A literal route is certain.
    assert server.confidence == 1.0
    assert server.detail == {"method": "GET", "path": "/engines/{}"}
    # The position is the HANDLER's name, not the decorator's line: the
    # decorator is where the route is declared but the function is what serves
    # it, and it is the function the EXPOSES edge attaches to. Line 7 is
    # `@app.get(...)`, line 8 column 5 is `read_engine`.
    assert (server.line, server.col) == (8, 5)

    client = by_role["client"]
    assert (client.mechanism, client.key) == ("http", "POST /ingest")
    # A client call is inferred from context, so it decays below 1.0.
    assert 0.0 < client.confidence < 1.0


def test_typescript_server_and_client():
    keys = {(p.role, p.key) for p in extract_typescript_boundaries(TYPESCRIPT)}
    assert keys == {("server", "GET /ingest"), ("client", "GET /engines/{}")}


def test_typescript_tsx_mode_parses_jsx():
    # The grammar differs; `tsx=True` must not choke on an element literal.
    assert extract_typescript_boundaries(b"const x = <div/>;", True) == []


def test_go_server_and_client():
    ports = {(p.role, p.key) for p in extract_go_boundaries(GO)}
    # net/http registers a path for every method, hence ANY.
    assert ("server", "ANY /engines/{}") in ports
    assert ("client", "GET /ingest") in ports


def test_rust_server():
    ports = extract_rust_boundaries(RUST)
    assert [(p.role, p.key) for p in ports] == [("server", "GET /engines/{}")]


def test_yaml_openapi_paths_become_one_port_per_operation():
    ports = extract_yaml_boundaries(YAML)
    assert [(p.role, p.key, p.confidence) for p in ports] == [
        ("server", "GET /engines/{}", 1.0)
    ]
    assert ports[0].detail["source"] == "openapi"


def test_every_language_reduces_the_same_route_to_the_same_key():
    """The property the whole cross-language story rests on."""
    keys = {
        extract_python_boundaries(PYTHON)[0].key,
        next(p.key for p in extract_typescript_boundaries(TYPESCRIPT) if p.role == "client"),
        extract_rust_boundaries(RUST)[0].key,
        extract_yaml_boundaries(YAML)[0].key,
    }
    assert keys == {"GET /engines/{}"}


@pytest.mark.parametrize("name", sorted(EXTRACTORS))
def test_coordinates_are_one_based(name):
    source = {"python": PYTHON, "typescript": TYPESCRIPT, "go": GO, "rust": RUST, "yaml": YAML}[name]
    for port in EXTRACTORS[name](source):
        assert port.line >= 1, port
        assert port.col >= 1, port
