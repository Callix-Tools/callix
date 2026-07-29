---
sidebar_position: 5
---

# Go adapter

```python
from callix import GoAdapter

graph = GoAdapter().analyze("path/to/go-module")
GoAdapter(resolve=False).analyze(root)      # structure only
```

| Property | Value |
|---|---|
| Language id | `go` |
| Project marker | `go.mod` |
| Extensions | `.go` |
| Resolver | `go/packages` + `go/types` |
| External requirements | a Go toolchain on `PATH` |

## A different node scheme

A `MODULE` here is a **Go package** — a directory — and its qualified name is
the import path. That is deliberate: an internal import can then be bound by
looking the path up directly, with no name mangling in between.

```
module github.com/acme/svc     →  MODULE github.com/acme/svc
internal/store/store.go        →  MODULE github.com/acme/svc/internal/store
```

## What it extracts

Functions, methods with their receiver type in the qualified name, structs and
interfaces, type aliases, constants and variables, and imports. Embedded types
in structs and interfaces become `base` occurrences and resolve to
`INHERITS_FROM`.

Embedding is distinguished from generic constraints by term count: a type-set
union like `A | B` or `~int` has several terms in one element, real embedding
has exactly one.

## Dependency classification

Go's own rule applies — a standard-library import path has no dot in its first
segment, third-party paths start with a domain. Module paths come from `go.mod`,
from both block and single-line `require` directives.

## Boundaries

gin, chi and echo routes; `net/http` clients; gRPC stubs built through
`New<Service>Client`; Temporal activities; queue publish and subscribe.

## The resolver

`go/packages` and `go/types` are linked into the same Go c-archive as the
TypeScript bridge, so resolution is an in-process call rather than an LSP
conversation. Verified against `gopls`: the answers match down to the column.

Two things it does that are easy to miss:

- **Test files are type-checked.** `packages.Load` runs with `Tests: true`;
  without it every use-site in `*_test.go` stays unresolved. On gin that alone
  is the difference between 16.5% and 95% resolved.
- **Builtins have a position.** `len`, `make`, `append`, `panic`, the
  conversions, and `error.Error` live in universe scope and carry no source
  position. gopls points at `$GOROOT/src/builtin/builtin.go`, so the bridge
  loads the `builtin` package separately and records its declarations —
  including the methods of builtin interfaces. That takes gin from 95% to
  99.3%.

## Why Go is required

Unlike Python and TypeScript, Go's standard library is **sources in GOROOT**,
and `packages.Load` shells out to `go list`. It cannot be frozen into a wheel.
For a Go project this costs nothing: the toolchain is already there.
