<div align="center">

  <h1>callix</h1>

  <p>Polyglot code analysis in Rust. Parses Python, TypeScript, Go, Rust, PHP, C, C++ and YAML into a shared graph IR — with the type checkers linked in, so there are no language servers to install.</p>

  [![PyPI](https://img.shields.io/pypi/v/callix?color=blue)](https://pypi.org/project/callix/)
  [![Python](https://img.shields.io/pypi/pyversions/callix)](https://pypi.org/project/callix/)
  [![License](https://img.shields.io/github/license/Callix-Tools/callix)](LICENSE)
  [![CI](https://img.shields.io/github/actions/workflow/status/Callix-Tools/callix/CI.yml?label=CI)](https://github.com/Callix-Tools/callix/actions)

  [Documentation](https://callix-tools.github.io/callix/) · [Repository](https://github.com/Callix-Tools/callix) · [Issues](https://github.com/Callix-Tools/callix/issues)

</div>

---

```
Repository → Language Adapter → Graph (IR) → your code
```

Nodes and directed relations, typed and deterministic: functions, classes,
modules, the calls between them, and the HTTP or gRPC boundaries where one
service meets another.

## Why

Code analysis usually asks you to assemble it — install a language server, put
it on `PATH`, wire up the transport, keep the versions in step. callix links
the analysis engines into the module instead:

| Language | Symbol resolution | Extra setup |
|---|---|---|
| Python | [ty](https://github.com/astral-sh/ty), linked as a library | none |
| TypeScript | [typescript-go](https://github.com/microsoft/typescript-go), linked as a Go c-archive | none |
| Go | `go/packages` + `go/types`, same c-archive | a Go toolchain |
| Rust | `rust-analyzer scip` index, decoded natively | `rust-analyzer` and Cargo |
| PHP | a symbol table built from the sources | none |
| C / C++ | a symbol table over the parsed sources | none |
| YAML | none — it declares no symbols | none |

Python and TypeScript need nothing beyond the wheel — the checkers and their
standard-library stubs are inside it. Go and Rust use the toolchain the project
already has, because a standard library shipped as *sources* cannot be frozen
into a wheel. PHP and the C family have no checker worth linking and no widely
installed indexer worth shelling out to — every precise C/C++ option requires a
`compile_commands.json`, which almost no repository ships — so callix builds a
symbol table from the sources and reports `degraded`, honest about being a symbol
table and not a type checker.

If a project *does* have a `compile_commands.json`, `ClangScipResolver` drives
[scip-clang](https://github.com/sourcegraph/scip-clang) — real Clang semantic
analysis, overloads resolved and all — as an opt-in `resolver=` for `CAdapter`
and `CppAdapter`. See [C and C++](https://callix-tools.github.io/callix/docs/adapters/c-family#clangscipresolver-the-real-thing-if-you-have-a-compdb).

## Installation

```bash
pip install callix
```

Python ≥ 3.10. Wheels for Linux glibc and musl (x86_64, aarch64) and macOS
(arm64, x86_64);
[Windows is not supported](https://callix-tools.github.io/callix/docs/getting-started/installation).

## Usage

```python
from callix import NodeKind, PythonAdapter

graph = PythonAdapter().analyze("path/to/project")

print(len(graph.nodes), len(graph.relations))
for node in graph.nodes_by_kind(NodeKind.FUNCTION):
    print(node.qualified_name, graph.callers(node.id))

graph.to_json(indent=2)          # serialization
old.diff(new)                    # structural diff
```

The same call works for every other language — `TypeScriptAdapter`, `GoAdapter`,
`RustAdapter`, `PhpAdapter`, `CAdapter`, `CppAdapter`, `YamlAdapter` — and each
discovers its own project roots, so a monorepo works without configuration.

Symbol resolution is on by default. `resolve=False` gives a structural graph;
`graph.metadata["resolver_status"]` always says how complete the result is.

A Python server declaring `@app.get("/users/{id}")` and a TypeScript client
calling `fetch("/users/1")` produce the **same** boundary node, so merging
their graphs links the two services. See
[Cross-language analysis](https://callix-tools.github.io/callix/docs/guides/cross-language).

## Documentation

<https://callix-tools.github.io/callix/>

- [Installation](https://callix-tools.github.io/callix/docs/getting-started/installation) · [Quick start](https://callix-tools.github.io/callix/docs/getting-started/quick-start) · [Concepts](https://callix-tools.github.io/callix/docs/getting-started/concepts)
- [Adapters](https://callix-tools.github.io/callix/docs/adapters/overview) and [how the resolvers work](https://callix-tools.github.io/callix/docs/adapters/resolvers)
- [Graph model](https://callix-tools.github.io/callix/docs/graph-model/nodes) · [API reference](https://callix-tools.github.io/callix/docs/api-reference/callix)
- [Migrating from graphlens](https://callix-tools.github.io/callix/docs/project/migrating-from-graphlens)

## Benchmarks

<!-- BENCH:START -->

_Last run: **2026-07-29 13:55 UTC** · callix `main` · runner `Linux x86_64` · single cold run, indicative only._

| Project | Lang | Commit | LOC | Files | Nodes | Relations | Time | Peak RSS | KLOC/s | Resolver | Resolved |
|---|---|---|--:|--:|--:|--:|--:|--:|--:|:--|--:|
| [apache/superset](https://github.com/apache/superset) | python | `c83fb2b` | 399 519 | 1 886 | 179 307 | 379 535 | 17.2s | 1,004 MB | 23.2 | ok | 84% of 281 667 (10s) |
| [colinhacks/zod](https://github.com/colinhacks/zod) | typescript | `1fb56a5` | 74 194 | 404 | 15 236 | 27 578 | 2.3s | 420 MB | 32.2 | ok | 89% of 17 067 (1s) |
| [gin-gonic/gin](https://github.com/gin-gonic/gin) | go | `73726dc` | 23 672 | 98 | 11 688 | 15 646 | 5.4s | 933 MB | 4.4 | ok | 99% of 12 755 (0s) |
| [casdoor/casdoor](https://github.com/casdoor/casdoor) | go | `696bcf0` | 86 898 | 458 | 32 606 | 42 295 | 35.1s | 4,207 MB | 2.5 | ok | 100% of 33 504 (7s) |
| [gohugoio/hugo](https://github.com/gohugoio/hugo) | go | `4d22555` | 224 821 | 897 | 75 619 | 100 542 | 16.8s | 4,432 MB | 13.3 | ok | 98% of 78 036 (4s) |
| [BurntSushi/ripgrep](https://github.com/BurntSushi/ripgrep) | rust | `4649aa9` | 50 275 | 98 | 5 420 | 15 318 | 17.1s | 1,076 MB | 2.9 | ok | 99% of 11 668 (0s) |
| [tokio-rs/axum](https://github.com/tokio-rs/axum) | rust | `c59208c` | 43 653 | 296 | 8 162 | 15 095 | 88.1s | 3,124 MB | 0.5 | ok | 87% of 10 030 (0s) |
| [astral-sh/ruff](https://github.com/astral-sh/ruff) | rust | `6686f63` | 687 409 | 1 870 | 70 186 | 221 119 | 230.8s | 6,006 MB | 3.0 | ok | 100% of 159 277 (2s) |
| **Total** | | | **1 590 441** | | **398 224** | | **413.0s** | | **3.9** | | **92% of 604 004** |

<sub>Peak RSS measured via `getrusage.max` (largest single process — ours or a resolver subprocess; no cgroup counter was readable, so this is not the tree total). KLOC/s = analysed thousands-of-lines per second. Generated by [`benchmarks/run_benchmarks.py`](benchmarks/run_benchmarks.py).</sub>

<!-- BENCH:END -->

Speed-ups against [graphlens](https://github.com/Neko1313/graphlens), the
Python implementation this replaces: **5.2×** on apache/superset, **3.3×** on
colinhacks/zod, **2.6×** on gin-gonic/gin. Details and method in
[Benchmarks](https://callix-tools.github.io/callix/docs/project/benchmarks).

## Versioning

Semver covers the graph contract — the node and relation kinds, the two id
formulas, 1-based spans, node ordering, the serialization format — and the
Python API as generated into the committed `.pyi`. It does not cover what the
resolvers answer: the Python one is a pinned `0.0.x` release of ty, the Rust
one is whatever `rust-analyzer` the machine has. A patch release may change
which `CALLS` edges appear. What is watched instead is the aggregate share of
resolved queries, recorded per project in the baselines.

Full statement, including which changes need a major release:
[Versioning](https://callix-tools.github.io/callix/docs/project/versioning).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development loop, the build
constraints, and how releases are cut.

## License

MIT
