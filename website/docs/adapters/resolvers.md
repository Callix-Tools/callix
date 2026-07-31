---
sidebar_position: 2
---

# Resolvers

A resolver answers "what is defined at this position". This is the part that
usually forces you to install and configure a language server; here two real
type checkers are compiled into the module, two more use the toolchain the
project already has, and three languages get a symbol table built from their own
sources.

| Language | Engine | Where it runs | External requirement | Best status |
|---|---|---|---|---|
| Python | ty | in-process, as a library | none | `ok` |
| TypeScript | typescript-go | in-process, via a Go c-archive | none | `ok` |
| Go | `go/packages` + `go/types` | in-process, same c-archive | Go toolchain | `ok` |
| Rust | `rust-analyzer scip` | subprocess, index decoded natively | rust-analyzer, Cargo | `ok` |
| PHP | a symbol table over the parsed sources | in-process | none | `degraded` |
| C / C++ | a symbol table over the parsed sources | in-process | none | `degraded` |
| C / C++ (opt-in) | `scip-clang`, via `ClangScipResolver` | subprocess, index decoded natively | scip-clang, a `compile_commands.json` | `ok` |
| YAML | — | — | — | `ok`, with zero queries |

The last column is what the graph can report at best, and it is what to read
before trusting an edge. A type checker knows which overload a call selected and
which class's method it landed on; a symbol table knows only which declarations
were visible from that file. See
[PHP](./php.md#what-it-cannot-answer) and
[C and C++](./c-family.md#resolution-and-why-it-is-degraded) for what each one
cannot answer.

YAML's `ok` is not a rounding of `unavailable`: nothing failed and nothing is
missing, because a YAML document declares no symbols to resolve.

## Why two are linked in and two are not

Python and TypeScript ship their standard-library type information as data —
typeshed and `lib.d.ts` — so both can be bundled and the whole checker linked
into the module. Nothing is spawned, and there is no JSON-RPC.

Go and Rust cannot work that way. Type checking Go reads the standard library's
**sources** from GOROOT and shells out to `go list`; resolving Rust needs a
real Cargo workspace load. Neither can be frozen into a wheel, so both rely on
the toolchain the project already has.

## Why PHP and the C family get a symbol table instead

Neither has a checker in a form callix could link, and neither has an indexer
common enough to shell out to. For C and C++ the obstacle is sharper: every
precise option — scip-clang, clangd's index, libclang — hard-requires a
`compile_commands.json`, which is a by-product of running the build and which a
survey of twelve prominent C/C++ repositories found checked into **none** of
them. A resolver built on it would report `unavailable` on very nearly every
folder someone points callix at.

So both build a table from the sources already parsed and report `degraded`.
That is not a placeholder — PHP resolves by autoload map and namespace, C and
C++ by declaration visibility through `#include`, and a table encodes exactly
that — but it is not type inference, and the status says so.

Both accept `resolver=` if you have something better; see
[Custom resolvers and parsers](../guides/custom-resolvers.md). For C and C++
specifically, "something better" does not have to be one you write:
[`ClangScipResolver`](./c-family.md#clangscipresolver-the-real-thing-if-you-have-a-compdb)
ships in the module and drives `scip-clang` — real Clang semantic analysis,
overloads resolved and all — over a project's own `compile_commands.json` when
one exists. It reports `ok`, not `degraded`, because it is not a symbol table.

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
- **PHP, C, C++** — the table is built from the parse callix already did, so
  resolution adds a fraction of a second rather than a phase.

The batch SCIP index is a deliberate choice over an interactive
`rust-analyzer` server: the server keeps the whole workspace's analysis state
resident and balloons to tens of gigabytes on large projects, while the index
is written once and read statically. callix decodes it without a protobuf
runtime — only the symbol, the roles bitfield and the range start are needed.

## When the toolchain is missing

Nothing fails. The adapter returns a structural graph and says so in
`graph.metadata["resolver_status"]`. Pass `strict=True` to `analyze()` if you
would rather have an `AdapterError`.

`strict=True` therefore always raises for PHP, C and C++, whose best status is
`degraded`. That is the intended reading: `strict` means "refuse a graph you
cannot vouch for", and there callix cannot.

For Rust there is one more wrinkle worth knowing: `rust-analyzer` on `PATH` is
usually a rustup proxy that honours a project's `rust-toolchain.toml`. If that
pinned toolchain lacks the component — ruff pins one that does — the proxy
exits with `Unknown binary` and resolution would silently yield nothing. callix
asks `rustup which` from the project root first and falls back to the default
toolchain rather than accepting the empty answer.
