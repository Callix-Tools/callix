---
sidebar_position: 1
---

# Installation

```bash
pip install callix
```

Python **≥ 3.10** is required. The wheel is built against the stable ABI
(`abi3-py310`), so one wheel per platform covers 3.10 and every version after
it — including ones released after the wheel was.

:::info What is on PyPI today is 1.0.0
That release carries all **eight** adapters — Python, TypeScript, Go, Rust, PHP,
C, C++ and YAML — the drop to Python 3.10, and the musl wheels below. Everything
on this site documents it.

`0.1.0`, the only earlier release, had the first four adapters and needed
Python ≥ 3.13.
:::

## What is already inside

The Python and TypeScript type checkers are linked into the extension module,
together with the standard-library stubs they need — typeshed for Python,
`lib.d.ts` for TypeScript. Nothing else has to be installed, and no language
server is spawned.

That is also why the download is large: the wheel is around 21 MB and the
extension module inside it about 61 MB. Two type checkers, typeshed and
`lib.d.ts` are what fill it; eight tree-sitter grammars are a rounding error
beside them.

## What you have to provide

| Analysing | Requires |
|---|---|
| Python | nothing |
| TypeScript | nothing |
| Go | a Go toolchain on `PATH` |
| Rust | `rust-analyzer` and Cargo on `PATH` |
| PHP | nothing |
| C, C++ | nothing |
| YAML | nothing |

Go's and Rust's standard libraries cannot be compiled in: type checking reads
their sources from GOROOT and from a real Cargo workspace. In practice this
costs nothing — a Go project already has Go, and a Rust project already has the
toolchain.

Without them the adapter still returns a **structural** graph and reports the
shortfall in `graph.metadata["resolver_status"]`, rather than failing.

PHP, C and C++ need nothing because their resolution is a symbol table built
from the sources callix already parsed. It reports `degraded`, which is the
honest label for a table rather than a checker; see
[Resolvers](../adapters/resolvers.md).

## Supported platforms

Wheels are published for:

- Linux `x86_64` and `aarch64`, glibc (manylinux 2_28) and musl
  (musllinux 1_2) — so Alpine-based CI images work without building
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
