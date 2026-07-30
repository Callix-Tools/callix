---
sidebar_position: 2
---

# Quick start

Analyse a project and look at what came out:

```python
from callix import NodeKind, PythonAdapter

graph = PythonAdapter().analyze("path/to/project")

print(len(graph.nodes), len(graph.relations))

for node in graph.nodes_by_kind(NodeKind.FUNCTION):
    print(node.qualified_name, graph.callers(node.id))
```

`analyze()` takes a path and returns a `Graph`. It is a pure function of the
source tree: no daemon, no cache directory, no side effects.

## Swap the language

Every adapter shares one interface, so nothing above changes:

```python
from callix import (
    CAdapter, CppAdapter, GoAdapter, PhpAdapter,
    PythonAdapter, RustAdapter, TypeScriptAdapter, YamlAdapter,
)

graph = TypeScriptAdapter().analyze("path/to/ts-project")
graph = GoAdapter().analyze("path/to/go-module")
graph = RustAdapter().analyze("path/to/workspace")
graph = PhpAdapter().analyze("path/to/php-project")
graph = CAdapter().analyze("path/to/c-project")
```

Each one discovers its own project roots — `pyproject.toml`, `package.json`,
`go.mod`, `Cargo.toml`, `composer.json`, `CMakeLists.txt` — and handles monorepos
with several of them.

## Structure only

Symbol resolution is the expensive half. Turn it off when you only need the
skeleton:

```python
graph = PythonAdapter(resolve=False).analyze(root)
graph.metadata["resolver_status"]   # 'unavailable'
```

You get `PROJECT`, `MODULE`, `FILE`, `CLASS`, `FUNCTION`, `IMPORT` and their
containment edges, but no `CALLS`, `REFERENCES`, `HAS_TYPE` or `INHERITS_FROM`
— those are what resolution produces.

## Check the result before trusting it

A graph always tells you how complete it is:

```python
graph.metadata["resolver_status"]    # 'ok' | 'degraded' | 'unavailable'
graph.metadata["resolver_metrics"]
# {'queries': 28, 'resolved': 25, 'internal': 12, 'external': 13,
#  'unresolved': 3, 'seconds': 0.034, 'resolved_pct': 89.3}
```

Without this, a fast run that resolved nothing is indistinguishable from a fast
run that resolved everything. Pass `strict=True` to `analyze()` to raise
`AdapterError` instead of returning a degraded graph.

## Save it

```python
graph.to_json(indent=2)              # str
Graph.from_json(text)                # back again
old.diff(new)                        # structural diff
```

The format matches graphlens byte for byte, and node IDs are deterministic, so
two runs over unchanged source produce identical output.
