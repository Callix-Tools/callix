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

_Last run: **2026-07-30 12:01 UTC** · callix `main` · runner `Linux x86_64` · single cold run, indicative only._

| Project | Lang | Commit | LOC | Files | Nodes | Relations | Time | Peak RSS | KLOC/s | Resolver | Resolved |
|---|---|---|--:|--:|--:|--:|--:|--:|--:|:--|--:|
| [apache/superset](https://github.com/apache/superset) | python | `c83fb2b` | 399 519 | 1 886 | 181 320 | 370 764 | 19.4s | 1,026 MB | 20.6 | ok | 85% of 281 667 (11s) |
| [colinhacks/zod](https://github.com/colinhacks/zod) | typescript | `1fb56a5` | 74 194 | 404 | 15 332 | 27 334 | 2.5s | 419 MB | 30.2 | ok | 89% of 17 067 (1s) |
| [gin-gonic/gin](https://github.com/gin-gonic/gin) | go | `73726dc` | 23 374 | 97 | 13 109 | 19 424 | 6.1s | 948 MB | 3.8 | ok | 100% of 13 916 (1s) |
| [casdoor/casdoor](https://github.com/casdoor/casdoor) | go | `696bcf0` | 86 898 | 458 | 33 324 | 58 107 | 35.7s | 4,240 MB | 2.4 | ok | 100% of 36 633 (10s) |
| [gohugoio/hugo](https://github.com/gohugoio/hugo) | go | `4d22555` | 224 821 | 897 | 75 036 | 130 508 | 22.0s | 4,559 MB | 10.2 | ok | 98% of 87 043 (6s) |
| [BurntSushi/ripgrep](https://github.com/BurntSushi/ripgrep) | rust | `4649aa9` | 50 275 | 98 | 7 787 | 22 852 | 12.8s | 1,077 MB | 3.9 | ok | 99% of 16 156 (0s) |
| [tokio-rs/axum](https://github.com/tokio-rs/axum) | rust | `c59208c` | 43 653 | 296 | 10 627 | 21 118 | 69.0s | 3,123 MB | 0.6 | ok | 87% of 12 625 (0s) |
| [astral-sh/ruff](https://github.com/astral-sh/ruff) | rust | `6686f63` | 687 409 | 1 870 | 102 595 | 330 597 | 176.0s | 6,007 MB | 3.9 | ok | 100% of 216 936 (2s) |
| [laravel/framework](https://github.com/laravel/framework) | php | `92a7072` | 540 800 | 3 016 | 83 164 | 291 129 | 17.8s | 376 MB | 30.4 | degraded | 70% of 283 108 (1s) |
| [redis/redis](https://github.com/redis/redis) | c | `5279a8d` | 374 211 | 786 | 156 896 | 200 372 | 7.0s | 294 MB | 53.8 | degraded | 100% of 161 477 (0s) |
| [google/leveldb](https://github.com/google/leveldb) | cpp | `99b3c03` | 28 280 | 131 | 12 235 | 20 600 | 0.9s | 77 MB | 32.5 | degraded | 100% of 14 322 (0s) |
| [facebook/rocksdb](https://github.com/facebook/rocksdb) | cpp | `3b44608` | 774 485 | 1 342 | 300 996 | 513 211 | 26.9s | 718 MB | 28.8 | degraded | 100% of 433 814 (0s) |
| [APIs-guru/openapi-directory](https://github.com/APIs-guru/openapi-directory) | yaml | `959d727` | 1 268 530 | 971 | 11 812 | 16 795 | 5.8s | 115 MB | 219.8 | ok | — |
| [argoproj/argo-cd](https://github.com/argoproj/argo-cd) | yaml | `564b949` | 322 262 | 2 201 | 2 220 | 2 246 | 1.4s | 91 MB | 222.5 | ok | — |
| **Total** | | | **4 898 711** | | **1 006 453** | | **403.3s** | | **12.1** | | **91% of 1 574 764** |

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
