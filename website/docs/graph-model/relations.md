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
| `RESOLVES_TO` | import → module or external symbol (in Go with the `span` of the import path); also, with a `span` and a `resolution`, import → what a name it brings in resolves to: a definition or a module | structure (Rust: the name, by path) and resolution |
| `DEPENDS_ON` | project → dependency | structure |
| `CALLS` | function → callee | resolution |
| `REFERENCES` | declaration → symbol read or written | resolution |
| `HAS_TYPE` | declaration → its annotated type | resolution |
| `INHERITS_FROM` | type → base type or trait | resolution |
| `EXPOSES` | function → boundary | boundaries |
| `CONSUMES` | function → boundary | boundaries |
| `COMMUNICATES_WITH` | service → service | `Graph.link_boundaries()` |

The split matters when reading a graph: the first group exists in any run, the
second only when resolution ran, the third comes from boundary extraction.

`COMMUNICATES_WITH` is the one edge no adapter produces on its own — it needs
two services in the same graph. Merge them, then call
[`link_boundaries()`](../guides/cross-language.md#linking-the-two-sides).

## Metadata

Resolution records where the edge was written:

```python
relation.metadata["span"]       # the occurrence's span
relation.metadata["access"]     # 'read' | 'write', on REFERENCES
relation.metadata["file"]       # only when the edge was written in another file
                                # than its source node's (see below)
relation.metadata["resolution"] # how the edge was found (see below)
relation.metadata["via"]        # on a 'flow' edge: the holder the callee was followed through
relation.metadata["ambiguous"]  # on a 'name' edge: several declarations carried the name
```

A structural `RESOLVES_TO` may carry `span` too: the C family writes the path literal of an `#include` there.

### How the edge was found

`resolution` is one of `definition`, `value`, `flow`, `external`, `unlocated` or
`name`, and it is what separates an edge the resolver located from one it guessed:

| Value | Means |
|---|---|
| `definition` | The backend located a definition in the project and the edge points at it. |
| `value` | A call whose target is a variable, a parameter or a field: the backend found what the callee was read from, not the callee. |
| `flow` | A `value` call followed through assignments, arguments and returns to a callable it can be shown to hold; `via` names the holder. Flow-insensitive: a holder assigned two callables gets an edge to each, except that a parameter reassigned by a top-level statement before it is called (PHP) holds only what was assigned at that call. |
| `external` | The definition is outside the project (standard library, a dependency). |
| `unlocated` | The backend answered with a place callix could not bind to a node (a position inside an import statement, a file callix does not know). The start of a known Python or TypeScript file is bound to its MODULE. A call that lands on a MODULE is `unlocated` too, but points at the MODULE. |
| `name` | Matched by name in a symbol table (PHP, C, C++), or by path (Rust `use` paths and `mod x;`), with no type information. |

An edge written by a visitor rather than by resolution (`DECLARES`, `CONTAINS`,
`IMPORTS`) carries none of these. [How accurate resolution is](../project/accuracy.md)
says what each language's backend follows and what it does not.

`file` exists because definitions that share a qualified name are one node, and
that node lives in the first file that declared it. Go allows any number of
`init` functions in a package, so a call written in the second one is an edge
from a node in the first file. Where an edge was written, then, is `file` when
present and the source node's `file_path` otherwise; the LSIF and SCIP exporters
place a reference by it.

### What a definition depends on besides its body

A route handler is wired up outside its body, so the walk reads those places
too. What the definition *depends on* is an edge from the decorated definition;
a decorator is *called* by the scope the definition sits in (the module, the
class), because that is when it runs:

| Written as | Edge |
|---|---|
| `@audit`, `@router.get("/x")`, `@Guard`, `@Get(":id")` | from the definition: `REFERENCES` to `audit`, `router`, `get`/`Get`. From the scope that holds it: `CALLS` to `audit`, `get`/`Get` |
| `db=Depends(get_db)`, `opt = dep()` (a parameter default) | `CALLS` to `Depends`, `REFERENCES` to `get_db` |
| `@Inject("x") id` (a parameter decorator, TypeScript) | `CALLS` to `Inject` |
| `class A(Base, metaclass=Meta)` | `REFERENCES` to `Meta` |
| `session.query(Model).filter(cond()).all()` | one edge per name in the chain: `session`, `query`, `Model`, `filter`, `cond`, `all` |

`metadata["decorators"]` lists them by what is called: `["router.get", "audit"]`.
The edge is `decorated -> decorator`, so the router behind a handler is one hop
away, and a handler's `EXPOSES` edge starts at the handler, not at its file.

What is **not** followed: a symbol the resolver cannot name stays an external
symbol keyed by its position (`fastapi.Depends` is a site, not a name), and a
router mounted in another module than the one that builds it keeps the prefix it
was built with.

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

## Duplicates are meaningful — when they carry metadata

Relations are a list, not a set. Two `CALLS` between the same pair of nodes
from two different call sites are two edges, distinguished by their `span`
metadata. Diffing respects that: the edge key includes canonicalized metadata,
so neither the duplicates collapse nor a metadata-only change goes unnoticed.

A **structural** edge is the opposite case, and the rule is exactly one per
`(source, target, kind)`. It has no metadata, so a second copy would say nothing
the first did not: a visitor emits one `DECLARES` per occurrence while the node
it points at is deduplicated by qualified name, which meant a variable assigned
eight times produced eight identical edges — 7.7% of all edges on real projects.
Every adapter drops those before returning. An edge that carries metadata is
exempt, because there the duplicate records *where* it was observed.
