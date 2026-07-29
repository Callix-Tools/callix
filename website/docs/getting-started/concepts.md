---
sidebar_position: 3
---

# Core concepts

## Graph

A `Graph` holds **nodes** keyed by ID and an ordered list of **relations**.
Order is part of the value: nodes come out in insertion order, so serializing
the same source twice gives identical bytes.

## Adapter

An adapter turns a source tree into a graph. It owns project discovery, file
collection, parsing, and the resolution pass. There is one per language, and
they share the same surface:

```python
adapter.language()          # 'python'
adapter.file_extensions()   # {'.py', '.pyi'}
adapter.can_handle(root)    # bool
adapter.collect_files(root) # list[Path]
adapter.analyze(root, files=None, *, strict=False)
```

## Occurrence

Adapters do not create call or reference edges while walking the syntax tree.
They record **occurrences** — a position plus a role (`call`, `read`, `write`,
`annotation`, `base`) — and a later pass asks the resolver where each one is
defined, turning the answer into an edge.

This split is why the same visitor serves both a structural run and a fully
resolved one.

## Resolver

A resolver answers "what is defined at this position". Each language has one,
and it is the only part that needs real type information. The result carries an
**origin** — `internal`, `stdlib`, `third_party` or `unknown` — which decides
whether the edge lands on a node in the graph or on an `EXTERNAL_SYMBOL`.

An edge is never dropped: a target outside the graph still gets a node.

## Boundary

A **boundary** is a contract between services that no compiler checks: an HTTP
route, a gRPC method, a queue topic, a Temporal activity. Adapters detect both
sides — the server `EXPOSES`, the client `CONSUMES` — and the node's ID comes
from the mechanism and the normalized key alone.

That last detail is what makes cross-language analysis work: a Python server
declaring `@app.get("/users/{id}")` and a TypeScript client calling
`fetch("/users/1")` produce **the same node**, because both keys normalize to
`GET /users/{}`.

## Deterministic IDs

Every node ID is `sha256("{project}::{kind}::{qualified_name}")[:16]`. Nothing
about the machine, the run, or the absolute path enters it. Two consequences:

- graphs from different runs and different machines can be diffed and merged;
- incremental updates are possible, because an unchanged declaration keeps its
  identity.
