---
sidebar_position: 2
---

# Resolvers

A resolver answers "what is defined at this position". This is the part that
usually forces you to install and configure a language server; in callix two of
the four are compiled into the module.

| Language | Engine | Where it runs | External requirement |
|---|---|---|---|
| Python | ty | in-process, as a library | none |
| TypeScript | typescript-go | in-process, via a Go c-archive | none |
| Go | `go/packages` + `go/types` | in-process, same c-archive | Go toolchain |
| Rust | `rust-analyzer scip` | subprocess, index decoded natively | rust-analyzer, Cargo |

## Why two are linked in and two are not

Python and TypeScript ship their standard-library type information as data —
typeshed and `lib.d.ts` — so both can be bundled and the whole checker linked
into the module. Nothing is spawned, and there is no JSON-RPC.

Go and Rust cannot work that way. Type checking Go reads the standard library's
**sources** from GOROOT and shells out to `go list`; resolving Rust needs a
real Cargo workspace load. Neither can be frozen into a wheel, so both rely on
the toolchain the project already has.

## Cost

The engine dominates the runtime, and it dominates differently per language:

- **Python / TypeScript** — resolution is a function call. On a 400k-line
  Python project the whole analysis lands around 9 seconds.
- **Go** — `packages.Load` type-checks the project up front; that is where the
  time and the memory go.
- **Rust** — `rust-analyzer scip` is a subprocess that indexes the entire
  workspace before a single question is asked. On ruff that is about 96 seconds
  of the ~100 the analysis takes. The lookups afterwards cost well under a
  second.

The batch SCIP index is a deliberate choice over an interactive
`rust-analyzer` server: the server keeps the whole workspace's analysis state
resident and balloons to tens of gigabytes on large projects, while the index
is written once and read statically. callix decodes it without a protobuf
runtime — only the symbol, the roles bitfield and the range start are needed.

## When the toolchain is missing

Nothing fails. The adapter returns a structural graph and says so in
`graph.metadata["resolver_status"]`. Pass `strict=True` to `analyze()` if you
would rather have an `AdapterError`.

For Rust there is one more wrinkle worth knowing: `rust-analyzer` on `PATH` is
usually a rustup proxy that honours a project's `rust-toolchain.toml`. If that
pinned toolchain lacks the component — ruff pins one that does — the proxy
exits with `Unknown binary` and resolution would silently yield nothing. callix
asks `rustup which` from the project root first and falls back to the default
toolchain rather than accepting the empty answer.
