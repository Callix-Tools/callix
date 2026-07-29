# Contributing

## Development loop

Everything goes through [Task](https://taskfile.dev) — `taskfile.dist.yaml`.

```bash
task install                     # create the venv (Python 3.13) and install maturin
task                             # build → generate .pyi → run example/test.py
task example -- example/foo.py   # run a different script
task lint                        # clippy with -D warnings, same as CI
task check                       # compile check without building the module
task test                        # cargo test plus a smoke run of the example
task --list                      # everything else
```

The first build is slow — the ty/ruff tree is about 200 crates — and
incremental afterwards. Rust ≥ 1.95, Go ≥ 1.26, and network access on the
first build, which clones typescript-go into `.ts-go/` (~370 MB, surviving
`cargo clean`).

**Run cargo through the tasks, not directly.** The Taskfile sets
`PYO3_PYTHON` to `.venv/bin/python`: `abi3-py313` refuses an interpreter older
than 3.13, and pyo3 would otherwise pick whatever `python3` comes first on
`PATH` — which on a CI runner is regularly older. Export the variable yourself
if you want to call cargo by hand.

**Every cargo task shares one profile**, release by default. In debug the
embedded ty is roughly 15× slower — resolving graphlens takes 12.5s against
0.8s — so the dev loop turns into waiting, and linting in debug while building
in release would compile those 200 crates twice. `DEBUG=1` switches all of
them together and is meant for a debugger, not for measuring anything.

`cargo fmt` is deliberately not a task: formatting is hand-tuned in places and
it would rewrite the code into its own style. Strictness is clippy's job
instead — `task lint` runs it with `-D warnings`, and so does CI.

## Stubs

`python/callix/_core/__init__.pyi` is generated from the Rust doc comments by
`pyo3-stub-gen` and **is committed**. CI fails when the generated file drifts
from the committed one, so run `task stubs` after changing a signature and
commit the result. Never edit it by hand.

Two quirks live in `src/bin/stub_gen.rs`: module constants declared through
`m.add(...)` are appended manually because the generator does not see them, and
an exception's base class gets a `builtins.` prefix that has to be stripped.

## Correctness

```bash
task golden              # compare the fixture graphs against tests/golden
task golden UPDATE=1     # rewrite them, once a change is understood
```

`tests/fixtures/` holds a small project per language, each exercising the parts
of the contract that language supports. Two levels are frozen in
`tests/golden/`: the whole structural graph byte for byte — no resolver, no
toolchain, runs anywhere — and, for the resolved run, counts per node and
relation kind plus the resolver metrics. Full resolved graphs would be too
brittle to freeze, but a kind that silently stops being emitted, or a resolved
share that drops, still shows up.

Those golden files were cross-validated against
[graphlens](https://github.com/Neko1313/graphlens), the Python implementation
callix replaces, while that project still ran: all four fixtures matched it
byte for byte at the moment they were frozen. That is worth knowing because
graphlens is archived — the baseline was captured while it could still be
regenerated.

Since then the Go and Rust adapters have deliberately moved **past** graphlens:
they now emit PARAMETER and ATTRIBUTE nodes, IMPORTS edges, and the annotation
and read/write occurrences that graphlens never produced for those languages.
So a fresh parity run against graphlens will show extra nodes and edges on
those two — that is the fix, not a regression.

The wide half of the net is [`tests/baseline/`](tests/baseline): a
deterministic fingerprint of each of the eight benchmark projects — totals,
counts per kind, and a per-file hash over the nodes. Whole graphs cannot be
frozen at that size, but a fingerprint still points at the file that moved.

```bash
task baseline                       # compare
task baseline:capture               # rewrite, cloning what is missing
task baseline -- --project astral-sh/ruff
```

Not part of `task test` or CI: it needs ~2 GB of checkouts and several minutes.
Run it when you touch a visitor, the graph model, or path handling.

[`tests/baseline/README.md`](tests/baseline/README.md) records the cross-check
against graphlens as of 2026-07-30, project by project — useful because that
comparison is not reproducible forever.

Every subtle bug found so far surfaced only this way — key ordering, a
`HashMap` where an `IndexMap` was required, path sorting that has to match
Python's component-wise `pathlib.Path` comparison. If you touch a visitor or
the graph model, run a parity check before trusting the change.

Two practical notes. Compare structure only with `Adapter(resolve=False)` on
this side and a stub resolver on graphlens's; the metrics block will differ
because callix skips the pass entirely. And re-snapshot the graphlens baseline
before believing a diff — a stale dump once produced a fake 448k-line
regression.

One divergence is intentional and applies to all four languages: an
unresolvable `EXTERNAL_SYMBOL` key includes the project-relative file path, so
expect diffs confined to those nodes. The reasoning is in
[the migration notes](https://callix-tools.github.io/callix/docs/project/migrating-from-graphlens).

## Build constraints

The embedded ty is what makes resolution fast, and it comes with costs that are
deliberate trade-offs rather than bugs to fix.

**Pinned to a ruff commit.** `ty_ide` and `ty_project` are marked
`publish = false` and never reach crates.io, so they are git dependencies with
`rev` pinned to the commit matching a ty tag (currently ty 0.0.52). Updating ty
means finding the new commit
(`gh api repos/astral-sh/ty/contents/ruff?ref=<tag>`), changing `rev` in
**every** `Cargo.toml` entry together, rebuilding, and verifying the result.
The API of those crates is internal and changes without notice.

**A fragile feature set.** Outside ruff's workspace the features are not
inherited, so `get-size2` is pinned to exactly `=0.10.0` with a hand-written
feature list: 0.10.3 moved to `compact_str` 0.10 while ruff is built against
0.9, and without the pin `ruff_python_ast` does not compile at all.

**Size and time.** `_core.abi3.so` is about 21 MB against roughly 2.5 MB
without ty, and a cold build takes tens of minutes — covered by the cargo cache
in CI and by incremental rebuilds locally.

**Windows.** The TypeScript and Go resolvers are a Go c-archive, which cgo
produces through mingw-w64 in GNU archive format; that does not link into the
MSVC target CPython expects. `build.rs` refuses up front rather than leaving a
linker error. Cross-compiling between architectures does work — `build.rs`
derives `GOOS`/`GOARCH` from the Rust target and hands cgo an explicit
`-arch` — which is how the Intel macOS wheel is built on an Apple Silicon
runner.

Large SCIP indexes and Go builds respect `TMPDIR`; a small `/tmp` will fill.

## Documentation

The site lives in [`website/`](website) (Docusaurus) and deploys to GitHub
Pages on every push touching that directory.

```bash
cd website
pnpm install
pnpm start      # local preview
pnpm build      # what CI runs; fails on a broken link
```

## Benchmarks

`benchmarks/` clones real projects and measures analysis; see
[its README](benchmarks/README.md).

```bash
task bench:one PROJECT=gin-gonic/gin
task bench:run BENCH_LANG=rust
```

The table in the root README is generated — do not edit it by hand. The
`Benchmark` workflow refreshes and commits it.

## Releasing

Versions come from conventional commits via
[git-cliff](https://git-cliff.org): `cliff.toml` holds both the commit-parsing
rules and the `CHANGELOG.md` template. The `Release` workflow
(`workflow_dispatch`) runs the same tasks available locally:

```bash
task release:version                # the next version from the commits
task release:bump VERSION=0.2.0     # Cargo.toml + Cargo.lock
task release:changelog TAG=v0.2.0   # CHANGELOG.md
task release:commit TAG=v0.2.0      # commit + annotated tag
task release:push TAG=v0.2.0
```

The version lives **only** in `Cargo.toml` — `pyproject.toml` declares it
`dynamic` and maturin reads it from there.

Wheels are built by a per-platform matrix, because one wheel cannot cover every
platform with ty and the Go bridge linked inside: Linux x86_64 and aarch64
(manylinux 2_28, with Go installed into the container) and macOS arm64 and
x86_64. Everything else builds from the sdist and needs Go plus network access.

Publishing needs one secret, `PYPI_TOKEN`, and write permission for the
workflow so it can push the release commit and tag.
