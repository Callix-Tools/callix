---
slug: /
sidebar_position: 1
---

# Introduction

**callix** parses Python, TypeScript, Go and Rust projects — and the YAML that
wires them together — normalizing everything into a shared **graph IR** — typed nodes and directed relations — which it hands
to Python code for dependency analysis, navigation, and code-intelligence
tooling.

```
Repository → Language Adapter → Graph (IR) → your code
```

Everything of substance is written in Rust and ships inside one extension
module. The Python package is a facade of re-exports.

## What makes it different

Most code-analysis tooling asks you to assemble it: install a language server,
put it on `PATH`, configure the transport, keep the versions in step. callix
links the analysis engines **into the module itself**:

| Language | Symbol resolution | Extra setup |
|---|---|---|
| Python | [ty](https://github.com/astral-sh/ty), linked as a library | none |
| TypeScript | [typescript-go](https://github.com/microsoft/typescript-go), linked as a Go c-archive | none |
| Go | `go/packages` + `go/types`, same c-archive | a Go toolchain |
| Rust | `rust-analyzer scip` index, decoded natively | `rust-analyzer` and Cargo |

For Python and TypeScript that means `pip install callix` and nothing else —
the type checkers and their standard-library stubs (typeshed, `lib.d.ts`) are
inside the wheel. Go and Rust need their own toolchain, because a language's
standard library cannot be compiled in: type checking reads its sources from
GOROOT or from a real Cargo workspace.

## Scope and non-goals

callix produces a graph IR and stops there. Deliberately, it does **not**:

- **persist state or own a database** — the graph is a value you serialize,
  diff, or feed to a backend of your choosing;
- **watch the filesystem or re-index on its own** — a scan is a pure function
  of the source tree; deterministic node IDs make incremental updates possible,
  but the caller drives them;
- **compute embeddings or semantic search** — the graph is structural and
  type-aware, not a vector index;
- **provide a UI, a CLI, or an agent runtime.**

## Relation to graphlens

callix is a full rewrite of
[graphlens](https://github.com/Neko1313/graphlens) by the same author. The
public API is compatible — the same 14 node kinds, 12 relation kinds, the same
deterministic IDs and serialization format — so a graph produced by either one
reads in the other. What changed is that parsing, the graph models, boundary
extraction and symbol resolution all moved to Rust, and the resolvers stopped
being external processes you install separately.

See [Migrating from graphlens](./project/migrating-from-graphlens.md) for the
differences that matter in practice, and [Benchmarks](./project/benchmarks.md)
for the numbers.
