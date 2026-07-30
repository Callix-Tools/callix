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
| `http` | FastAPI / Flask / Starlette routes, Express and NestJS routes, gin / chi / echo, `net/http` and `ServeMux`, axum `.route()`, actix and rocket attributes, Laravel / Symfony / Slim routes, civetweb / libmicrohttpd / cpp-httplib / Crow / Drogon, an OpenAPI document, a Kubernetes Ingress | requests, httpx, `fetch`, axios, `net/http`, reqwest, Guzzle, `file_get_contents`, libcurl — and any named client |
| `grpc` | servicer classes, a C++ class inheriting `X::Service` | generated stubs (`<Service>Stub`, `New<Service>Client`, tonic clients, `X::NewStub`) |
| `queue` | `subscribe`, `consume`, `basic_consume`, `xreadgroup` | `publish`, `produce`, `emit`, `SendMessage`, `basic_publish`, `xadd`, `enqueue`, `delay`, Laravel job dispatches |
| `temporal` | `@activity.defn`, `RegisterActivity` | `execute_activity`, `ExecuteActivity` |

Per-language detail is on each adapter's page. Two of the rows are worth
expanding on, because they are where a key nearly fails to match:

- **A URL is rarely a literal in C.** `snprintf(url, n, "/engines/%ld", id)`
  builds it into a buffer, and the format string is reduced *whole* — stopping at
  the first letter of `%ld` would leave a `d` behind and the key would never meet
  a route. Same for a C++ `"/engines/" + std::to_string(id)`.
- **YAML declares boundaries no code states.** An OpenAPI path is a server-side
  route with confidence `1.0` and no handler to attach it to, which is what makes
  "does the implementation match the spec" a graph query.

That is not a hypothetical. Merge all eight of the repository's own fixtures and
one node — `http:GET /engines/{}` — carries six edges from six languages:

```
c           consumes  0.7   src/main.c::fetch_one
cpp         consumes  0.7   src/main.cpp::fetch_one(fixture::Identifier)
php         consumes  0.9   Fixture\fetchOne
python      consumes  0.9   pkg/service.py
typescript  consumes  0.9   src/service.ts
yaml        exposes   1.0   openapi.yaml
```

Five clients, written five different ways, and the OpenAPI document that
declares the route none of them names.

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
**confidence**: `1.0` when the contract was stated outright, lower when part of
it was inferred. A client-side HTTP call is `0.85`–`0.9`, a queue topic `0.7`;
the bands and what they mean are in
[Boundaries](../graph-model/boundaries.md#confidence).

## What is not recognized

Detection is syntactic, and the list above is what the built-in extractors
actually match. Known gaps, stated plainly:

| Not detected | Why |
|---|---|
| Django `urlpatterns` | a list of `path()` calls — no decorator, no verb |
| Next.js, Remix, SvelteKit route files | the route is the directory tree, not a call |
| tRPC | a typed proxy; no URL exists in the source |
| GraphQL clients | one endpoint, meaning lives in operation names |
| `axios({method, url})` | the verb is inside a config object |
| Kafka via `send()` — kafka-python, KafkaJS | the verb is also `res.send(...)` in every Express handler and `conn.send(...)` on every socket |

A route built at runtime from a variable, or a topic read from configuration,
will not be found either.

What *is* recognized no longer depends on naming. A route is told from a request
by whether the call binds a handler — `app.get(path, handler)` against
`client.get(url)` — so an Express app called `srv`, an axios instance called
`api`, Angular's `this.http.get(...)` and a gin router reached through a struct
field all work. The receiver-name whitelists that used to gate this were both
too narrow and too loose: they missed every client not named `axios`, and they
turned `request.Header.Get("X-Forwarded-Prefix")` into a route.

None of this is permanent: [a boundary
extractor](./custom-resolvers.md#a-boundary-extractor) is a plain Python object,
and the first three rows are exactly the cases that need one — they call for a
different *kind* of extractor, not one more pattern in the same match.
