---
sidebar_position: 1
---

# callix

Everything is re-exported from the top-level package; the native module lives
at `callix._core` and is not meant to be imported directly.

```python
from callix import Graph, NodeKind, PythonAdapter
```

## Adapters

```python
PythonAdapter(dep_parsers=None, resolver=None, *, resolve=True)
TypeScriptAdapter(*, resolve=True)
GoAdapter(*, resolve=True)
RustAdapter(*, resolve=True)
```

Shared methods:

| Method | Returns |
|---|---|
| `language()` | `str` — `'python'`, `'typescript'`, `'go'`, `'rust'` |
| `file_extensions()` | `set[str]` |
| `can_handle(project_root)` | `bool` |
| `collect_files(project_root)` | `list[Path]` |
| `analyze(project_root, files=None, *, strict=False)` | `Graph` |

## Graph

**Building**

| Method | Notes |
|---|---|
| `add_node(node)` | `DuplicateNodeError` on an ID collision |
| `add_relation(relation)` | |
| `merge(other, *, allow_shared=False)` | `allow_shared` permits identical nodes to coincide |

**Serialization**

`to_dict()`, `to_json(indent=None)`, `from_dict(data)`, `from_json(text)`,
`diff(other)`.

**Edges**

`outgoing(node_id)`, `incoming(node_id)`.

**Queries**

`callees(id)`, `callers(id)`, `references_to(id)`, `neighbors(id, depth=1)`,
`nodes_by_kind(kind)`, `nodes_in_file(path)`, `find_by_name(name)`,
`subgraph(ids)`, `file_subgraph(path)`.

**Attributes**

`nodes` — an insertion-ordered mapping of ID to node. `relations` — a list.
`metadata` — a dict.

## Resolvers

```python
TyResolver(base_prefix=None)     # Python, wraps the embedded ty
TsResolver()                     # TypeScript
GoResolver()                     # Go
RustResolver()                   # Rust
```

Each implements `prepare(project_root, files=None)`,
`resolve_all(queries)`, `definition_at(file, line, col)` and `status()`.

## Functions

| Function | Purpose |
|---|---|
| `make_node_id(project_name, qualified_name, kind)` | the ID formula |
| `make_boundary_id(mechanism, key)` | the boundary ID formula |
| `normalize_http_path(path)` | the shared key normalizer |
| `diff_graphs(old, new)` | what `Graph.diff` calls |
| `node_to_dict` / `node_from_dict` | single-node serialization |
| `relation_to_dict` / `relation_from_dict` | single-edge serialization |
| `ensure_schema_version(data)` | raises on an unsupported version |

Per-language helpers are exported too — `is_python_project`,
`find_rust_roots`, `classify_go_import`, `extract_rust_boundaries` and so on.

## Constants

```python
callix.__version__
callix.SCHEMA_VERSION
callix.RESOLVER_STATUS_KEY     # 'resolver_status'
callix.RESOLVER_METRICS_KEY    # 'resolver_metrics'
```
