---
sidebar_position: 2
---

# Relations

A relation is a directed edge that refers to its endpoints by ID.

```python
relation.source_id
relation.target_id
relation.kind        # RelationKind
relation.metadata    # dict[str, object]
```

## The 12 kinds

| Kind | From → To | Produced by |
|---|---|---|
| `CONTAINS` | project → module, module → file | structure |
| `DECLARES` | file → declaration | structure |
| `IMPORTS` | file → import | structure |
| `RESOLVES_TO` | import → module or external symbol | structure |
| `DEPENDS_ON` | project → dependency | structure |
| `CALLS` | function → callee | resolution |
| `REFERENCES` | declaration → symbol read or written | resolution |
| `HAS_TYPE` | declaration → its annotated type | resolution |
| `INHERITS_FROM` | type → base type or trait | resolution |
| `EXPOSES` | function → boundary | boundaries |
| `CONSUMES` | function → boundary | boundaries |
| `COMMUNICATES_WITH` | service → service | linking, downstream |

The split matters when reading a graph: the first group exists in any run, the
second only when resolution ran, the third comes from boundary extraction.

## Metadata

Resolution records where the edge was written:

```python
relation.metadata["span"]      # the occurrence's span
relation.metadata["access"]    # 'read' | 'write', on REFERENCES
```

Boundary edges carry the contract:

```python
relation.metadata["mechanism"]   # 'http' | 'grpc' | 'queue' | 'temporal'
relation.metadata["key"]         # 'GET /users/{}'
relation.metadata["role"]        # 'server' | 'client'
relation.metadata["confidence"]  # 1.0 for a literal, less when inferred
relation.metadata["line"], relation.metadata["col"]
```

Plus per-mechanism detail: `method` and `path` for HTTP, `service` and `method`
for gRPC, `topic` for queues, `activity` for Temporal.

## Duplicates are meaningful

Relations are a list, not a set. Two `CALLS` between the same pair of nodes
from two different call sites are two edges, distinguished by their `span`
metadata. Diffing respects that: the edge key includes canonicalized metadata,
so neither the duplicates collapse nor a metadata-only change goes unnoticed.
