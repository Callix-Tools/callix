---
sidebar_position: 2
---

# Benchmarks

Two different questions get measured, and they answer differently.

## Throughput

The live table lives in the
[repository README](https://github.com/Callix-Tools/callix#benchmarks) and is
refreshed by CI. It analyses eight real projects — apache/superset,
colinhacks/zod, gin-gonic/gin, casdoor/casdoor, gohugoio/hugo,
BurntSushi/ripgrep, tokio-rs/axum, astral-sh/ruff — roughly 1.6 million lines
in total, and reports lines per second, peak memory and the resolved share.

Throughput splits sharply by how the resolver is wired:

| Language | Order of magnitude | Why |
|---|---|---|
| Python | ~40 KLOC/s | ty is a function call |
| TypeScript | ~33 KLOC/s | typescript-go is linked in |
| Go | ~2–10 KLOC/s | `packages.Load` type-checks the project up front |
| Rust | ~0.5–3 KLOC/s | dominated by the `rust-analyzer scip` subprocess |

For Rust the number is mostly a measurement of rust-analyzer. On ruff the index
build is about 96 seconds of the ~100 the whole analysis takes.

## Against graphlens

Measured per language on the same machine, same projects, comparing the two
implementations directly:

| Project | Language | graphlens | callix | Speed-up |
|---|---|---|---|---|
| graphlens itself, 151 files | Python | 5.40s | 1.14s | **4.7×** |
| apache/superset, 2440 files | Python | 87.48s | 16.72s | **5.2×** |
| colinhacks/zod, 287 files | TypeScript | 4.29s | 1.29s | **3.3×** |
| gin-gonic/gin, 98 files | Go | 4.37s | 1.71s | **2.6×** |
| BurntSushi/ripgrep, 110 files | Rust | 8.03s | 6.94s | 1.16× |
| astral-sh/ruff, 1902 files | Rust | 118.56s | 102.22s | 1.16× |

The resolution phase alone — the part that used to be JSON-RPC — moved further
than the totals suggest: on superset 39.1s → 8.9s, on gin 3.68s → 0.46s, on
ruff 5.60s → 0.62s.

Rust is the outlier for a structural reason: both implementations shell out to
the same `rust-analyzer scip`, and that subprocess is ~86% of the wall clock.
The phases callix actually owns went from 18 seconds to 5.7 on ruff; the total
barely moved because the other 96 seconds belong to somebody else.

## Resolution quality

Equal or near-equal everywhere, which is the point — speed that costs accuracy
would not be worth reporting.

| Project | graphlens | callix |
|---|---|---|
| apache/superset | 352,634 / 418,944 | identical |
| astral-sh/ruff | 169,792 / 170,232 (99.7%) | identical |
| gin-gonic/gin | 13,089 / 13,106 (99.9%) | 13,018 / 13,106 (99.3%) |

Go is the one place callix resolves slightly less. Those 88 positions are ones
`gopls` reconstructs beyond what plain `go/types` information carries; all of
them were traced to universe-scope edge cases.

## Reproducing

```bash
task bench:list
task bench:one PROJECT=gin-gonic/gin
task bench:run BENCH_LANG=rust
```

Numbers come from a single cold run on a shared CI machine — treat them as
indicative, not microbenchmark-grade. Peak memory is only read from the cgroup
counter inside a container; elsewhere it falls back to `getrusage` and reports
the largest single process rather than the tree total.
