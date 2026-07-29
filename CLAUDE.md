# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Language

All code, comments, docs, commit messages and error strings in this repository
are **English**. Keep it that way when adding code.

## Commands

Everything goes through [Task](https://taskfile.dev) — `taskfile.dist.yaml`.
Run cargo through the tasks rather than directly (see *Build gotchas* below).

```bash
task                             # build → regenerate .pyi → run example/test.py
task install                     # create .venv (Python 3.13) + maturin
task build                       # maturin develop, release profile
task lint                        # clippy with -D warnings (CI gate)
task check                       # cargo check, no module build
task test                        # cargo test + example smoke run
task stubs / task stubs:check    # regenerate / verify committed .pyi
task example -- example/foo.py   # run a different script
DEBUG=1 task ...                 # switch every cargo task to the debug profile
```

There is no test suite yet: `cargo test` compiles but has no tests, and
correctness is currently established by parity runs against graphlens (below).

`CONTRIBUTING.md` covers the same ground for humans; the README is deliberately
short and links out to the docs site in `website/`.

Benchmarks (`benchmarks/Taskfile.yaml`, included as `bench:`):

```bash
task bench:list [BENCH_LANG=rust]
task bench:one PROJECT=gin-gonic/gin
task bench:run [BENCH_LANG=go]
task bench:render VERSION=0.1.0   # splices the table into README.md
```

## Architecture

callix is a Rust rewrite of [graphlens](https://github.com/Neko1313/graphlens).
Everything of substance lives in the Rust extension module `callix._core`;
`python/callix/` is a facade of re-exports plus a thin `TyResolver` wrapper.

### The graph contract

`src/node.rs`, `src/relation.rs`, `src/graph.rs`, `src/ids.rs`, `src/span.rs`.
14 `NodeKind`s, 12 `RelationKind`s, 1-based spans, and IDs that must stay
deterministic: `sha256("{project}::{kind}::{qualified_name}")[:16]`. Boundary
IDs deliberately depend only on `mechanism` + normalized key, which is what
makes a server in one language and a client in another collapse into one node.

Two ordering constraints are load-bearing and easy to break:

- `Graph` stores nodes in an `IndexMap`, not a `HashMap` — insertion order is
  part of the output and a `HashMap` silently reshuffles `CONTAINS` edges.
- `src/roots.rs::sort_paths` compares paths **component by component**, the
  way Python's `sorted()` over `pathlib.Path` does. Sorting by the whole
  string diverges on neighbours like `src/v4-mini/…` vs `src/v4/…`.

### Anatomy of a language adapter

Each of `src/python/`, `src/typescript/`, `src/golang/`, `src/rustlang/`
follows the same layout:

| File | Role |
|---|---|
| `mod.rs` | thread-local tree-sitter parser, re-exports |
| `detector.rs` / `deps.rs` | project roots, manifest parsing, import classification |
| `visitor.rs` | CST walk → nodes, `DECLARES`/`CONTAINS`/`RESOLVES_TO` edges, occurrences |
| `boundary.rs` | cross-language ports via tree-sitter queries |
| `resolver.rs` | symbol resolution backend |
| `adapter.rs` | orchestration of `analyze()` |

**Visitors never create CALLS / REFERENCES / HAS_TYPE / INHERITS_FROM edges.**
They emit `OccurrenceRef`s (`src/occurrence.rs`) with a role — `call`, `read`,
`write`, `annotation`, `base` — which the resolution pass turns into edges.

`analyze()` always runs three phases, and the order is not cosmetic:

1. **Structure** for every sub-root, no resolution. The `SpanIndex` built next
   has to span the whole workspace, or cross-root definitions have nothing to
   bind to.
2. **One resolver for the whole call.** Per-root resolvers would both lose
   cross-root references and re-index the workspace once per root.
3. **Boundaries**, so BOUNDARY nodes land in the graph after resolver edges.

Resolution maps a definition position back to a node through
`SpanIndex::lookup_name` (name spans, not full extents). Anything that misses
falls through to an `EXTERNAL_SYMBOL` so the edge is never lost.

### Node schemes differ per language

Do not assume Python's shape elsewhere. `MODULE` is a dotted name in Python and
TypeScript, a **package directory** in Go (its `qualified_name` equals the
import path, so internal imports bind by direct lookup), and a Rust module path
in `rustlang`. Rust dispatches `impl` blocks in a second pass because a type may
be declared below its `impl`.

### Resolvers: what is linked in and what is not

| Language | Backend | External requirement |
|---|---|---|
| Python | `ty` linked as a library (`src/python/ty_embedded.rs`) | none |
| TypeScript | typescript-go via a Go c-archive (`go/bridge.go`) | none |
| Go | `go/packages` + `go/types` via the same c-archive (`go/bridge_go.go`) | Go toolchain |
| Rust | `rust-analyzer scip` subprocess + own SCIP decoder (`src/rustlang/scip.rs`) | rust-analyzer + Cargo |

Go and Rust cannot be compiled in: both need their standard library's sources
and a real workspace load. `build.rs` clones typescript-go into `.ts-go/`
(~370 MB, survives `cargo clean`) and builds `go/` into a c-archive. That
c-archive is why **Windows is unsupported** — cgo emits a mingw-w64 GNU
archive that will not link into the MSVC target; `build.rs` refuses up front.
It also derives GOOS/GOARCH from the Rust target so an Apple Intel wheel can
be cross-built on an Apple Silicon runner, and passes `-buildvcs=false`
because `go build` otherwise fails where git will not read the checkout (a
manylinux container mounts the workspace under another uid).

Resolvers are swappable from Python — any object with `prepare`/`resolve_all`/
`status` — as are dependency parsers (`can_parse`/`parse`). Rust calls them
through the ordinary Python protocol.

## Parity with graphlens

This is the project's correctness harness. graphlens lives at
`~/project/graphlens` with its own venv at `~/project/graphlens/.venv/bin/python`
(callix's venv does not have graphlens's tree-sitter grammars installed).

Method: build the same graph both ways → `to_dict()` →
`json.dumps(sort_keys=True)` → `diff`. Every subtle bug found so far (key
ordering, HashMap vs IndexMap, path sorting) surfaced only this way. When
comparing structure only, use `Adapter(resolve=False)` on the callix side and a
stub resolver on graphlens's; the metrics block will differ because callix skips
the pass entirely, and that is expected.

Re-snapshot graphlens baselines before trusting a diff — a stale dump once
produced a fake 448k-line regression.

**One intentional divergence, applied to all four languages.** When the resolver
finds a definition it cannot name, the synthetic `EXTERNAL_SYMBOL` key includes
the project-relative file path (`{role}@{path}:{line}:{col}`). graphlens omits
the path, which merges distinct sites that share coordinates — 98% of external
nodes on apache/superset. Expect diffs confined to those nodes' ids/names.

## Build gotchas

- **`PYO3_PYTHON`** is set globally in the Taskfile to `.venv/bin/python`.
  `abi3-py313` refuses anything older than 3.13, and bare `cargo` otherwise
  picks the first `python3` on PATH.
- **All cargo tasks share one profile**, release by default. The ty/ruff tree is
  ~200 crates; linting in debug while building in release compiles it twice.
- **`get-size2` is pinned to exactly the version ruff uses** (`=0.10.3` for ty
  0.0.65) with a hand-written feature list copied from ruff's root manifest.
  `get-size2` and `compact_str` move in lockstep — 0.10.0 implements `GetSize`
  for `compact_str` 0.9, 0.10.3 for 0.10 — so resolving the wrong one leaves
  `ruff_python_ast` without a `GetSize` impl for `CompactString` and it does
  not compile.
- **ty comes from a git `rev`** pinned across several `Cargo.toml` entries. The
  rev for a ty release is the `ruff` submodule pointer at that tag in
  `astral-sh/ty` (`gh api repos/astral-sh/ty/contents/ruff?ref=<tag> --jq .sha`);
  re-derive the rev already pinned to check the method before trusting a new
  one. Change every entry together, re-pin `get-size2`, `cargo fetch` to catch
  a conflict without rustc, then rebuild. The MSRV is ruff's — 1.95 as of ty
  0.0.65 — which is why `release.yml` pins `rust-toolchain: stable` on
  `maturin-action`: the build containers ship their own Rust.
- Large SCIP indexes and Go builds respect `TMPDIR`; a small `/tmp` will fill.
- `cargo fmt` is deliberately not a task — formatting is hand-tuned in places.

## Stubs and release

`python/callix/_core/__init__.pyi` is generated by `src/bin/stub_gen.rs` from
the Rust doc comments and **is committed**; CI fails on drift. Module constants
and an exception-base-class fixup are appended manually there because
pyo3-stub-gen 0.23 does not emit them correctly.

Versions live **only** in `Cargo.toml` (`pyproject.toml` declares it `dynamic`).
`cliff.toml` drives both conventional-commit parsing and the changelog template;
its `[remote.github]` owner/repo must match the real repository or git-cliff
panics on a 404. Wheels are built by a per-platform matrix in `release.yml` —
one wheel cannot cover every platform with ty and the Go bridge linked inside.
