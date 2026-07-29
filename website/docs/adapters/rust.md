---
sidebar_position: 6
---

# Rust adapter

```python
from callix import RustAdapter

graph = RustAdapter().analyze("path/to/workspace")
RustAdapter(resolve=False).analyze(root)    # structure only
```

| Property | Value |
|---|---|
| Language id | `rust` |
| Project marker | `Cargo.toml` |
| Extensions | `.rs` |
| Resolver | `rust-analyzer scip` index |
| External requirements | `rust-analyzer` and Cargo on `PATH` |

Workspaces with several members are handled: every `Cargo.toml` under the path
is a crate root, and one index serves all of them.

## Module names

A `MODULE` is a Rust module path derived from the file's place under `src/`:

```
src/net/http.rs   →  crate_name::net::http
src/lib.rs        →  crate_name
```

Trailing `lib`, `main` and `mod` are dropped — they name the enclosing module,
not themselves. Inline `mod foo { … }` blocks add their own segment while the
visitor is inside them.

## What it extracts

Functions, structs, enums, traits and unions, type aliases, constants and
statics, `impl` blocks with their methods, and `use` declarations.

`impl` blocks are dispatched in a **second pass**. Rust does not require a type
to be declared above its `impl`, so an `impl Trait for Type` written before
`struct Type` would otherwise find no type node and lose the `base`
occurrence that becomes `INHERITS_FROM`.

## Dependency classification

`crate`, `self`, `super` and the crate's own name are internal; `std`, `core`,
`alloc`, `proc_macro` and `test` are the standard library; anything listed in
`dependencies`, `dev-dependencies` or `build-dependencies` is third-party.
Hyphens are normalized to underscores, since `Cargo.toml` says `tree-sitter`
where the code says `tree_sitter`.

## Boundaries

axum `.route()` routes — where the verb comes from the handler wrapper, as in
`.route("/x", get(handler))` — actix and rocket `#[get("/x")]` attributes,
reqwest clients, tonic gRPC stubs, and queues.

## The resolver

Resolution runs on the batch index produced by `rust-analyzer scip`, not on an
interactive LSP server. The server keeps the whole workspace's analysis state
resident and grows to tens of gigabytes on large projects; the index is written
once and read statically.

callix decodes SCIP itself, without a protobuf runtime — only three fields per
occurrence are needed: the symbol, the roles bitfield, and the start of the
range.

:::note Where the time goes
The index build dominates. On astral-sh/ruff — 1,900 files — the subprocess
takes around 96 seconds of the roughly 100 the analysis needs; the lookups
afterwards cost under a second. Rust throughput in the
[benchmarks](../project/benchmarks.md) is therefore mostly a measurement of
rust-analyzer, not of callix.
:::

## Toolchain pinning

`rust-analyzer` on `PATH` is usually a rustup proxy honouring the project's
`rust-toolchain.toml`. When that pinned toolchain lacks the component — ruff
pins one that does — the proxy exits with `Unknown binary` and resolution would
silently return nothing. callix asks `rustup which` from the project root
first, then falls back to the default toolchain.
