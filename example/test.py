"""An end-to-end run: PythonAdapter analyses a whole project."""

import tempfile
from pathlib import Path

from callix import (
    Graph,
    NodeKind,
    PythonAdapter,
    RelationKind,
    __version__,
)

PROJECT = {
    "pyproject.toml": """\
[project]
name = "demo"
dependencies = ["httpx"]
""",
    "src/demo/__init__.py": """\
from demo.core import Engine, run

__all__ = ["Engine", "run"]
""",
    "src/demo/core.py": '''\
from __future__ import annotations

import os

import httpx
from fastapi import FastAPI

app = FastAPI()


class Base:
    """The base engine."""

    def ping(self) -> str:
        return "pong"


class Engine(Base):
    def __init__(self, name: str) -> None:
        self.name = name

    def describe(self) -> str:
        return f"{self.name}: {self.ping()}"


def run(engine: Engine) -> str:
    return engine.describe() + os.sep


@app.get("/engines/{name}")
def read_engine(name: str) -> str:
    return run(Engine(name))


def push(payload: str) -> str:
    return httpx.post("/ingest", json=payload).text
''',
}


print("callix", __version__)

with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    for relative, text in PROJECT.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    # The default resolver is the embedded ty; no external binary needed
    adapter = PythonAdapter()
    print("  can_handle =", adapter.can_handle(root))
    print("  files found:", len(adapter.collect_files(root)))

    graph = adapter.analyze(root)
    print(graph)

    counts = {
        kind.value: len(graph.nodes_by_kind(kind))
        for kind in (
            NodeKind.PROJECT,
            NodeKind.MODULE,
            NodeKind.FILE,
            NodeKind.CLASS,
            NodeKind.METHOD,
            NodeKind.FUNCTION,
            NodeKind.IMPORT,
            NodeKind.EXTERNAL_SYMBOL,
        )
    }
    print("  nodes:", counts)

    imports = graph.nodes_by_kind(NodeKind.IMPORT)
    print("  imports:", sorted((n.name, n.metadata["origin"]) for n in imports))

    project = graph.nodes_by_kind(NodeKind.PROJECT)[0]
    top = [
        graph.nodes[r.target_id].qualified_name
        for r in graph.outgoing(project.id, RelationKind.CONTAINS)
    ]
    print("  PROJECT contains:", top)

    print("  resolver_status =", graph.metadata["resolver_status"])
    print("  resolver_metrics =", graph.metadata["resolver_metrics"])

    # Cross-file edges built by the resolver
    for kind in (RelationKind.CALLS, RelationKind.INHERITS_FROM, RelationKind.HAS_TYPE):
        edges = [r for r in graph.relations if r.kind == kind]
        print(f"  {kind.value:14s} {len(edges)}")

    engine = next(n for n in graph.nodes_by_kind(NodeKind.CLASS) if n.name == "Engine")
    bases = [graph.nodes[r.target_id].name for r in graph.outgoing(engine.id, RelationKind.INHERITS_FROM)]
    print("  Engine inherits:", bases)

    describe = next(n for n in graph.nodes_by_kind(NodeKind.METHOD) if n.name == "describe")
    print("  describe calls:", sorted({n.name for n in graph.callees(describe.id)}))

    # Cross-language boundaries: BOUNDARY nodes and EXPOSES / CONSUMES
    nodes = graph.nodes
    for rel in graph.relations:
        if rel.kind in (RelationKind.EXPOSES, RelationKind.CONSUMES):
            src, dst = nodes[rel.source_id], nodes[rel.target_id]
            print(f"  {src.name} --{rel.kind.value}--> {dst.qualified_name}")

    # The graph serializes and survives a round-trip
    restored = Graph.from_json(graph.to_json())
    print("  round-trip diff empty =", graph.diff(restored).is_empty)
