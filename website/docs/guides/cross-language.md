---
sidebar_position: 3
---

# Cross-language analysis

Two services in different languages have no compiler-checked link between them.
callix recovers it from the one thing both sides state literally: the contract.

## How it matches

Every adapter extracts **boundary ports** and normalizes their keys through the
same function. A node's ID is derived from the mechanism and the normalized key
alone — not from the project, the language, or the file — so both sides land on
one node.

```python
# service-a (Python)
@app.get("/users/{id}")
def read_user(id: int): ...

# service-b (TypeScript)
await fetch("/users/1")
```

Both keys normalize to `GET /users/{}`. Merge the two graphs and the server's
`EXPOSES` edge and the client's `CONSUMES` edge meet at a single `BOUNDARY`.

```python
from callix import PythonAdapter, TypeScriptAdapter

graph = PythonAdapter().analyze("services/api")
graph.merge(TypeScriptAdapter().analyze("services/web"), allow_shared=True)
```

## What is recognized

| Mechanism | Server side | Client side |
|---|---|---|
| `http` | FastAPI / Flask / Starlette routes, Express and NestJS routes, gin / chi / echo, axum `.route()`, actix and rocket attributes | requests, httpx, `fetch`, axios, `net/http`, reqwest |
| `grpc` | servicer classes | generated stubs (`<Service>Stub`, `New<Service>Client`, tonic clients) |
| `queue` | `subscribe` | `publish`, `produce`, `emit` |
| `temporal` | `@activity.defn`, `RegisterActivity` | `execute_activity`, `ExecuteActivity` |

## Linking the two sides

A boundary node joins the graphs, but the client function and the server
function are still only related *through* it. `link_boundaries()` makes the
relationship direct:

```python
added = graph.link_boundaries()          # returns the number of edges
graph.link_boundaries(min_confidence=0.8)  # or drop the weaker guesses
```

For every boundary it pairs each consumer with each provider and adds a
directed `consumer -> provider` edge of kind `COMMUNICATES_WITH`, carrying the
mechanism, the boundary's id and key, and a confidence that is the product of
the two sides'. A service that both exposes and consumes the same contract is
not linked to itself.

The pass is idempotent: re-running it after re-analysing part of the graph adds
nothing that is already there.

```python
for edge in graph.relations:
    if edge.kind is RelationKind.COMMUNICATES_WITH:
        print(graph.nodes[edge.source_id].qualified_name, "->",
              graph.nodes[edge.target_id].qualified_name,
              edge.metadata["boundary_key"])
# client.loadUser -> server.read_user GET /users/{}
```

## Reading the result

```python
from callix import NodeKind, RelationKind

for boundary in graph.nodes_by_kind(NodeKind.BOUNDARY):
    print(boundary.qualified_name)          # 'http:GET /users/{}'
    for edge in graph.incoming(boundary.id):
        side = "exposes" if edge.kind is RelationKind.EXPOSES else "consumes"
        print("  ", side, edge.source_id, edge.metadata["confidence"])
```

Each edge carries the mechanism, the key, the role, the source position, and a
**confidence**: `1.0` when the key came from a literal, lower when it was
inferred from context. A client-side HTTP call scores `0.85`, a queue topic
`0.7`.

## Limits

Detection is syntactic. A route built at runtime from a variable, a topic name
read from configuration, or a client wrapped in three layers of indirection
will not be found. The confidence value exists so you can filter on how much
the extractor actually saw.
