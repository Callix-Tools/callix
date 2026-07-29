---
sidebar_position: 1
---

# Installation

```bash
pip install callix
```

Python **≥ 3.13** is required. The wheel is built against the stable ABI
(`abi3-py313`), so one wheel per platform covers 3.13 and everything after it.

## What is already inside

The Python and TypeScript type checkers are linked into the extension module,
together with the standard-library stubs they need — typeshed for Python,
`lib.d.ts` for TypeScript. Nothing else has to be installed, and no language
server is spawned.

That is also why the module is large: `_core.abi3.so` is around 21 MB, against
roughly 2.5 MB without ty.

## What you have to provide

| Analysing | Requires |
|---|---|
| Python | nothing |
| TypeScript | nothing |
| Go | a Go toolchain on `PATH` |
| Rust | `rust-analyzer` and Cargo on `PATH` |

Go's and Rust's standard libraries cannot be compiled in: type checking reads
their sources from GOROOT and from a real Cargo workspace. In practice this
costs nothing — a Go project already has Go, and a Rust project already has the
toolchain.

Without them the adapter still returns a **structural** graph and reports the
shortfall in `graph.metadata["resolver_status"]`, rather than failing.

## Supported platforms

Wheels are published for:

- Linux `x86_64` and `aarch64` (manylinux 2_28)
- macOS `arm64` and `x86_64`

:::warning Windows is not supported
The TypeScript and Go resolvers are a Go c-archive, which cgo produces through
mingw-w64 in GNU archive format; that does not link into the MSVC target
CPython expects. Building from the sdist hits the same wall, so the build
refuses early with an explanation instead of leaving you with a linker error.
WSL works.
:::

## Building from source

Only needed on a platform without a wheel. It requires Rust ≥ 1.95, Go ≥ 1.26,
git and network access — the build clones typescript-go (~370 MB) and compiles
the ty tree, roughly 200 crates. Expect tens of minutes.

```bash
git clone https://github.com/Callix-Tools/callix
cd callix
task install
task build
```

See the repository README for the full development loop.
