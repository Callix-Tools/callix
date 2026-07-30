# callix load benchmarks

Measures analysis throughput on large, real-world projects. The numbers in the
root [`README.md`](../README.md) (the "Benchmarks" section) are produced by
this harness and refreshed by
[`.github/workflows/bench.yml`](../.github/workflows/bench.yml).

The benchmark only **reports** — it never fails CI. A project that cannot be
cloned or analysed is recorded as an error row and the run continues.

## What is measured

| Metric | Meaning |
|---|---|
| **LOC / Files** | What the adapter actually collected |
| **Nodes / Relations** | Size of the produced graph |
| **Time** | `adapter.analyze()` only; the clone is excluded |
| **Peak RSS** | High-water mark of the **whole process tree** — including the `rust-analyzer scip` subprocess — read from the cgroup (falling back to `getrusage`) |
| **KLOC/s** | Analysed thousands-of-lines per second |
| **Resolver** | Worst status across the project's languages (`ok` / `degraded` / `unavailable`) |

Timings come from a single cold run on a shared CI machine, so treat them as
**indicative**, not microbenchmark-grade. This goes double for Rust: roughly
85% of that time is the `rust-analyzer scip` subprocess rather than callix.

## Projects

Targets live in [`projects.json`](projects.json) — a size gradient per
adapter, each pinned to an upstream tag:

| Adapter | Projects |
|---|---|
| Python | apache/superset |
| TypeScript | colinhacks/zod |
| Go | gin-gonic/gin · casdoor/casdoor · gohugoio/hugo |
| Rust | BurntSushi/ripgrep · tokio-rs/axum · astral-sh/ruff |
| PHP | laravel/framework |
| C | redis/redis |
| C++ | google/leveldb · facebook/rocksdb |
| YAML | APIs-guru/openapi-directory · argoproj/argo-cd |

`c` and `cpp` are separate benchmark languages although they are one adapter
behind two classes, so each gets its own matrix job and its own row.

The two YAML entries measure different things on purpose. YAML declares no
symbols, so its whole contribution is boundaries — and on a tagged real-world
repository the yield is low, because Helm templates are not valid YAML and most
repositories hold few Ingresses: argo-cd's 2 201 files produce one boundary.
openapi-directory is the opposite, ~1 000 OpenAPI documents yielding some
10 800. One entry measures discovery and flavour detection at scale, the other
the extractor.

Edit the file freely: the workflow loops over every entry and the table order
follows the manifest. If a pinned ref has gone missing, the harness falls back
to the default branch and reports the actual SHA — a stale entry degrades
instead of breaking the run.

## Running it

```bash
task bench:list                          # what is available
task bench:list BENCH_LANG=rust          # one language only
task bench:one PROJECT=gin-gonic/gin     # a single project
task bench:run                           # everything
task bench:run BENCH_LANG=go             # every project of one language
task bench:show VERSION=0.1.0            # print the table
task bench:render VERSION=0.1.0          # splice it into the README
```

You need a built module (`task build`) and the resolvers' toolchains: Go for
Go projects, `rust-analyzer` and Cargo for Rust. Everything else needs nothing
extra — Python's and TypeScript's type checkers live inside the module, PHP and
the C family build a symbol table from the sources, and YAML resolves nothing.

Results are written one JSON file per project into `results/` and are
**committed**: `render` builds the table from the whole directory, so
languages that did not run this time keep their previous numbers instead of
dropping out of the table.

Peak memory is only read honestly from the cgroup — that is, in a container or
in CI. Locally it falls back to `getrusage`, which does not see subprocesses.
