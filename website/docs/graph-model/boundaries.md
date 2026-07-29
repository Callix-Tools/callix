---
sidebar_position: 3
---

# Boundaries

A boundary is a contract between services that no compiler resolves — an HTTP
route, a gRPC method, a queue topic, a Temporal activity. Each side of it is a
**port**: the server `EXPOSES`, the client `CONSUMES`.

## Key normalization

Matching only works if both sides reduce to the same string, so every adapter
runs its keys through one shared function. It:

- strips the scheme and host, the query and the fragment;
- collapses path parameters of every style — `{id}`, `:id`, `<int:id>`,
  `$id` — into `{}`;
- collapses bare numeric segments too, so `/users/1` meets `/users/{}`;
- drops the trailing slash, except at the root.

```
@app.get("/users/{id}")        →  GET /users/{}
router.get("/users/:id")       →  GET /users/{}
fetch("https://api/users/1")   →  GET /users/{}
```

A colon only counts as a parameter marker at the start of a segment, so
`/v1/users/123:activate` and `sha256:abc` survive intact.

## The node

```python
boundary.kind             # NodeKind.BOUNDARY
boundary.qualified_name   # 'http:GET /users/{}'
boundary.name             # 'GET /users/{}'
boundary.metadata         # {'mechanism': 'http', 'key': 'GET /users/{}'}
```

Its ID is derived from the mechanism and the key alone — no project, no
language — which is the whole mechanism behind cross-language matching. See
[Cross-language analysis](../guides/cross-language.md).

## Enclosure

A port is attached to the innermost `FUNCTION` or `METHOD` whose span contains
it, falling back to the `FILE` when the port sits at module level. That is what
makes "which handler serves this route" answerable from the graph.

## Confidence

Every edge carries one:

| Value | Meaning |
|---|---|
| `1.0` | a server-side route read from a literal |
| `0.9` | a Temporal activity |
| `0.85` | a client-side HTTP call, or a gRPC method |
| `0.75` | a queue consumer |
| `0.7` | a queue producer |

Detection is syntactic. A route assembled at runtime, or a topic read from
configuration, will not be found — the confidence value is there so you can
filter by how much the extractor actually saw.
