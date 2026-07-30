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

Every edge carries one, and it answers "how much of this contract did the
extractor actually see?" rather than "how sure are we the call happens":

| Band | What it means | Examples |
|---|---|---|
| `1.0` | the contract is stated outright, in a literal | a `@app.get("/users/{id}")` route, an OpenAPI path |
| `0.85`–`0.9` | a call whose target is a literal, but the peer is elsewhere | an HTTP client URL, a gRPC method, a Temporal activity |
| `0.7`–`0.8` | the mechanism is certain, the exact key inferred | a queue topic from a variable, a URL built by `snprintf` |
| `0.4` | the surface is known and the contract is not | libmicrohttpd's `MHD_start_daemon`: the port is visible, but the routes are decided inside a handler this cannot read, so the key is `ANY /*` |

The exact value is the extractor's, so it varies a little by language — a
server-side route is `1.0` everywhere, while an HTTP client is `0.9` in Python
and TypeScript and `0.85` in Go and Rust. Treat them as bands, not as a scale to
compare across languages.

Detection is syntactic. A route assembled at runtime, or a topic read from
configuration, will not be found — the confidence value is there so you can
filter by how much the extractor saw, and
[`link_boundaries()`](../guides/cross-language.md) multiplies the two sides', so
an edge is never more certain than its weaker half.
