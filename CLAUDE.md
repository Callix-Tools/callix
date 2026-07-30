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
task test                        # unit + golden + test:rust + example
task unit                        # pytest over tests/unit and tests/api
task golden                      # fixture graphs vs tests/golden (UPDATE=1 rewrites)
task test:rust                   # cargo test — 137 Rust-side tests
task baseline                    # fingerprints of 14 real projects (not in CI)
task stubs / task stubs:check    # regenerate / verify committed .pyi
task example -- example/foo.py   # run a different script
task docs:build                  # Docusaurus build — what the docs CI runs
DEBUG=1 task ...                 # switch every cargo task to the debug profile
```

### The regression net, in four layers

Correctness used to rest on parity runs against graphlens. graphlens is now
archived, so the harness stands on its own, and the four layers answer different
questions — a change to a visitor should be checked against the last two:

| Layer | What it is | Catches |
|---|---|---|
| `task test:rust` | 137 `#[test]`s in 17 files | enum wire values, `sort_paths`, the SCIP decoder, dependency parsers |
| `task unit` | pytest over `tests/unit` + `tests/api` | the public Python API that 1.0 freezes |
| `task golden` | whole graphs of `tests/fixtures/*`, frozen in `tests/golden/` | any change to any visitor, exactly |
| `task baseline` | fingerprints of 14 real repositories | what a three-file fixture cannot reach: scale, and the resolvers |

The first three run in CI and need no toolchain — every adapter runs with
`resolve=False`, which is also what makes them deterministic. Baselines need
~3 GB of clones and several minutes, so they are deliberately **not** in CI; run
them, with `--lang` to stay narrow, when you touch a visitor, the graph model or
path handling. `tests/baseline/README.md` says what each project pins down.

`CONTRIBUTING.md` covers the same ground for humans; the README is deliberately
short and links out to the docs site in `website/`.

Benchmarks (`benchmarks/Taskfile.yaml`, included as `bench:`):

```bash
task bench:list [BENCH_LANG=rust]
task bench:one PROJECT=gin-gonic/gin
task bench:run [BENCH_LANG=go]
task bench:render VERSION=1.0.0   # splices the table into README.md
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

Three further invariants, each of which was violated at some point:

- **A structural edge has no multiplicity.** Visitors emit one per occurrence
  while the node they point at is deduplicated by qualified name, so a variable
  assigned eight times produced eight identical `DECLARES` edges. Every adapter
  calls `graph.dedupe_structural_relations(py)` at the end of `analyze`; an
  edge that carries metadata records *where it was observed* and is exempt.
- **`merge` is all-or-nothing.** Ids are validated before any node is inserted,
  or a failed merge leaves part of the other graph behind depending on where
  the collision happened to sit. Relations are appended without deduplication:
  merge once per source graph.
- **Reading a payload raises `SerializationError` and nothing else.** Malformed
  JSON, a missing key and an unknown kind all funnel through it, so callers
  have one type to catch.

### What semver covers

**1.0.0 is released, so all of this is in force** — the window where breaking
changes were free is closed.

The graph contract above, the Python API in the committed `.pyi`, and
`SCHEMA_VERSION` — which will not move within 1.x, because
`ensure_schema_version` demands exact equality and a bump makes every stored
graph unreadable in both directions. Not covered: what the resolvers answer
(ty is a pinned `0.0.x`, rust-analyzer is whatever is installed), boundary
recall, or how exhaustively a given language is walked. Full text in
`website/docs/project/versioning.md`; thread-safety guarantees, which are
"single-threaded by construction — nothing releases the GIL", in
`website/docs/project/thread-safety.md`.

### Anatomy of a language adapter

Each of `src/python/`, `src/typescript/`, `src/golang/`, `src/rustlang/`,
`src/php/`, `src/cfamily/`, `src/yaml/` follows the same layout:

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

Two adapters do not fit that shape. **`src/cfamily/` runs four phases**: a scope
pass precedes everything, because `X::y` cannot be classified as a method or a
namespaced function until the set of class names is known, and the class is
usually declared in a different file from the definition. A survey pass then
reads every file into `FileFacts` and records declaring sites in a `DeclLedger`
before any node is written — a prototype in a header and its definition in a
source file are ONE node, and whichever file was read first would otherwise
decide its span. **`src/yaml/` has no visitor and no resolver at all**: it
declares no symbols, emits no occurrences, and contributes only FILE nodes,
dependencies and boundaries.

**A YAML document over ~1.27 MiB yields no boundaries, silently.** Past that
size tree-sitter-yaml stops producing the mapping structure the queries match.
The parse does not fail — `parse_tree` returns a tree and
`extract_yaml_boundaries` returns an empty list, so nothing raises and no status
degrades. It scales with tree size, not with any single node: a synthetic 4 MiB
file of small blocks fails the same way. Real specifications reach it — GitHub's
`api.github.com.yaml` is 8 MB and produces zero boundaries. Worth knowing before
concluding that an extractor is broken, and worth fixing by chunking large
documents rather than by touching the queries.

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
| PHP | a `SymbolTable` built from the sources (`src/php/resolver.rs`) | none |
| C / C++ | a `SymbolTable` over the parsed sources (`src/cfamily/resolver.rs`) | none |
| YAML | none — it declares no symbols | none |

PHP and the C family report **`degraded`**, deliberately: a symbol table is not
a type checker and says so. Every precise C/C++ option needs a
`compile_commands.json` that almost no repository ships, which is why there is
no subprocess to shell out to. Note that the C family's `unresolved` is always
0 — every miss becomes an `EXTERNAL_SYMBOL` rather than a dropped edge, so
`resolved == queries` and that ratio means nothing; the internal/external split
is the signal.

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

### One constructor, eight adapters — seven of them with a resolver

`src/resolver_slot.rs` holds `ResolverSlot<N>` — `Native(N)` / `Custom(Py<PyAny>)`
/ `Disabled` — plus the Python-protocol helpers and `coerce_ref`. All seven
resolving adapters take the same keyword-only arguments (`resolve`, `resolver`,
`dep_parsers`, `boundary_extractors`); `custom_declared` in
`src/dependencies.rs` is the `dep_parsers` half. Three things about it are
load-bearing:

- **The enum carries no trait bound.** `NativeResolver` (position-keyed:
  `resolve(path, line, col)`) is implemented by five backends — Python,
  TypeScript, Go, Rust and PHP — and the
  methods that need it live in a separate impl. That is what lets the C family
  use the same enum with a `SymbolTable` marker that resolves **by name** — a
  custom resolver there goes through a second, position-keyed path built on
  `apply_resolutions_rust`.
- **A custom resolver must answer every query.** Answers are zipped to queries by
  index, so a short list would silently shift them onto the wrong use-sites;
  `custom_resolve_all` raises instead.
- **`YamlAdapter` deliberately does not take `resolver`/`dep_parsers`.** It emits
  no occurrences and its Helm dependencies are per-file, so both arguments would
  be accepted and provably ignored. `tests/api/test_adapter_api.py` pins the
  `TypeError`.

## Parity with graphlens — historical

graphlens was the correctness harness until it was archived. It is **no longer
the reference**; the four layers above are. What remains is a record, in
`tests/baseline/README.md`, of the state at the moment its environment could
still run: for Python and TypeScript not one file-level hash differed, and for Go
and Rust every difference was an addition callix makes deliberately. If a future
change makes Python or TypeScript diverge beyond the `DEPENDENCY` nodes, that is
a regression and the table is the evidence of what came before.

`tests/baseline.py crosscheck` still implements the comparison and expects
graphlens at `~/project/graphlens` with its own venv (callix's venv does not have
graphlens's tree-sitter grammars). It reports, never gates. It also only ever
covered those four languages — graphlens has no PHP, C, C++ or YAML adapter, so
for those four the golden fixtures and the baselines are the whole net.

Two divergences are intentional. The synthetic `EXTERNAL_SYMBOL` key includes the
project-relative file path (`{role}@{path}:{line}:{col}`) where graphlens omits
it, merging distinct sites that share coordinates — 98% of external nodes on
apache/superset. And callix stores a **relative** `file_path` on every node,
where graphlens stored an absolute one on everything but the FILE node.

## Build gotchas

- **`PYO3_PYTHON`** is set globally in the Taskfile to `.venv/bin/python`, or
  bare `cargo` picks the first `python3` on PATH and the build depends on the
  machine. The wheel is `abi3-py310` and the floor is 3.10; the dev venv is
  pinned to 3.13 anyway, so a stub or resolver quirk surfaces here rather than in
  a user's environment.
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
