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
not themselves. An inline `mod foo { … }` block is a `MODULE` too (`crate_name::foo`),
like a file module, with no file or span of its own; it also adds its segment while the
visitor is inside it.

## What it extracts

Functions, structs, enums, traits and unions, type aliases, constants and
statics, `impl` blocks with their methods, and `use` declarations.

An enum variant is a `CLASS` named `Enum.Variant` (it is called like a tuple struct), and the named fields of a struct variant are `ATTRIBUTE` nodes under it; positional fields get no node. The associated types of an `impl` or a trait are `TYPE_ALIAS` nodes and their associated consts `VARIABLE` nodes, named `Type.Name` (`Trait.Name` in a trait). Two `impl` blocks on one type that declare the same associated name share one node, as their methods do. An item written inside a function body (a struct, enum, const, static, trait, impl, type alias or function) is a node qualified under the function, `module::f::Local`, and a method of an `impl` in a body is a `METHOD`. The calls and reads in a `const` or `static` initializer, an associated const or an enum discriminant are edges from that const, not from the function that happens to read it. A `macro_rules!` is a `FUNCTION` named `module::name` with `metadata["macro"] = "macro_rules"` (a macro written in a function body is qualified under the function), and the invocation of a macro the project defines, in any file, is a `CALLS` edge to it. An item-level invocation (`name!(..);` outside any function) is enclosed by the FILE, so its edge runs from the FILE to the macro's `FUNCTION` node. Invocations inside `impl` and `trait` bodies are not recorded. A macro from another file is not expanded; procedural macros are not covered.

`impl` blocks are dispatched in a **second pass**. Rust does not require a type
to be declared above its `impl`, so an `impl Trait for Type` written before
`struct Type` would otherwise find no type node and lose the `base`
occurrence that becomes `INHERITS_FROM`. When `Type` is not declared in the
project, the `INHERITS_FROM` comes from the file instead.

Every type name in a type position becomes a `HAS_TYPE`: generic arguments
(`Result<Matcher, Error>`), bounds and `where` clauses, `let`, alias, cast and
closure types, tuple-struct fields and enum payloads. The self type of an `impl`
and the generic arguments of its trait are `HAS_TYPE` edges from the file, not
from a module, because a module has no file of its own to place the edge in.
The name a generic parameter declares (`T` in `<T: Bound>`) is not a use.

The qualifiers of a path are references: `Foo` in `Foo::new()` becomes a `REFERENCES` edge to the type, `inner`, `crate` and `super` in `inner::f()`, `crate::flags::X` or `super::g()` to the `MODULE`, and the modules named by `pub(crate)`, `pub(super)` and `pub(in path)` the same way. They are enclosed like the annotation of the same position (the function, the parameter, the field), and by the file for a `mod`, a `use` or an `impl` header. `self` in a path and `<T as Trait>::` qualifiers are not recorded. A reference to a `MODULE` is not exported to LSIF or SCIP, because a Rust `MODULE` has no file to define it in.

Each name a `use` brings in is an edge of its own: `Circle` and `Square` in `use crate::shapes::{Circle, Square};` become `RESOLVES_TO` edges from the `IMPORT`, with the position of the name, to the type. Groups can nest, `as` points at the original name (not the alias), and `*`, `self`, `crate` and `super` are skipped. A path of the crate is bound by path in the structure pass and marked `name` (2018 relative paths like `use store::X` too); the rest (the standard library, dependencies, re-exports) is left to rust-analyzer. A `mod x;` is a `REFERENCES` edge from the file to the `MODULE`. Prefix segments of a use path, a `use` inside a function body and a `#[path]` module are not covered, and an edge to a `MODULE` is not exported.

Names in expression and pattern position are references too: a `const` or `static` read, a struct literal's type and its fields (`Cfg { n: 1 }`, and the shorthand `Cfg { n }`), the last segment of a path such as `Mode::A` or `u32::MAX`, and the paths and fields of match and `let` patterns. Each is a `REFERENCES` edge with `access: read` (`write` for an assignment target). A lowercase identifier in a pattern is a binding and a read of a local or a parameter is not an edge. The arguments of a macro the project defines, and the bare receiver of a field access (`x` in `x.f`), are not covered.

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
