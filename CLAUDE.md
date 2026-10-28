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
task accuracy [-- go/micro -v]   # resolvers vs hand-written call graphs; task accuracy:update raises the floor
task test:rust                   # cargo test --workspace — 137 core tests + exporters + the CLI end to end
task baseline                    # fingerprints of 14 real projects (not in CI)
task stubs / task stubs:check    # regenerate / verify committed .pyi
task example -- example/foo.py   # run a different script
task cli                         # build the `callix` binary
task docker / docker:slim        # build the images (callix:dev, callix:dev-slim)
task docker:smoke                # smoke-test callix:dev against tests/fixtures
task docs:build                  # Docusaurus build — what the docs CI runs
DEBUG=1 task ...                 # switch every cargo task to the debug profile
```

### The regression net, in five layers

Correctness used to rest on parity runs against graphlens. graphlens is now
archived, so the harness stands on its own, and the layers answer different
questions — a change to a visitor should be checked against the last three:

| Layer | What it is | Catches |
|---|---|---|
| `task test:rust` | `#[test]`s across the workspace | enum wire values, `sort_paths`, the SCIP decoder, dependency parsers, the C family's overload picking |
| `task unit` | pytest over `tests/unit`, `tests/api` and `tests/accuracy` | the public Python API that 1.0 freezes, and the accuracy floor below |
| `task golden` | whole graphs of `tests/fixtures/*`, frozen in `tests/golden/` | any change to any visitor, exactly |
| `task accuracy` | eight suites of small programs, each with the call graph a reader would draw | **whether a resolver is right**: the other layers freeze what callix produces, this one scores it |
| `task baseline` | fingerprints of 14 real repositories | what a three-file fixture cannot reach: scale, and the resolvers |

`tests/accuracy` is the only layer that can say a change made resolution *worse*.
`thresholds.json` is a ratchet — pytest fails below the recorded floor,
`task accuracy:update` only raises it, and `run.py --reset` is how a change that
trades one number for another says so. Ground truth is written from the
language's rules, not from callix's output; `tests/accuracy/README.md` has the
conventions. Cases the resolver fails are the reason they exist — do not bend a
`callgraph.json` to match the tool.

The golden, unit and baseline layers need no toolchain — every golden adapter runs with
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

## Dynamic workflows: which model does what

When work is orchestrated as a multi-agent workflow, the stages run on
different models, and the split is a rule, not a suggestion:

| Stage | Model | What it produces |
|---|---|---|
| **Design** | Opus | the approach: architecture, invariants at stake, which layers of the regression net must prove it |
| **Decomposition** | Sonnet | a list of tasks, each compact and self-explanatory — **5 to 10 file changes**, with the files named, the expected result stated and the check to run |
| **Execution** | Haiku | the changes themselves, one task per agent |

A task a Haiku agent cannot finish from its brief alone is a decomposition
defect: split it further or make it more explicit, rather than moving it up
to a larger model. A task over 10 file changes is split; one under 5 is merged
with a neighbour unless it stands alone.

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

In TypeScript a `namespace`/`module` block is a MODULE `<scope>.N` (`is_namespace`, a real span and `name_span`, DECLARED by its container), and the members of a type alias's object type are `Alias.member`; a re-declaration in the same file adds a `declared_at` span instead of a node. An overload signature, exported or not, is a `declared_at` span of its implementation, and a file-level `declare function` is a FUNCTION declared at every signature.

In Python and TypeScript a MODULE that has a file of its own (the `.py` file or
`__init__.py`; the `.ts` file or `index.*`) is placed at a zero-width `1:1` span in
that file, and `metadata["name_span"]` is the same span. A namespace package and a
directory without an index file have no file, so they stay unplaced. The placement is
decided in a pre-pass over the files (`ensure_module_chain` takes it), because an
ancestor package is created while its children are handled, before its own
`__init__.py` is reached.

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

### What an edge claims about how it was found

A resolver's answer is one of several quite different things, and the graph does
not call them all "resolved". Every `CALLS`/`REFERENCES`/`HAS_TYPE`/`INHERITS_FROM`
edge a resolution pass writes has `metadata["resolution"]` — `definition`,
`value`, `flow`, `external`, `unlocated` or `name` — set in one place,
`src/resolution.rs::Resolution::classify` (the C family's table writes `name`, and
`ambiguous: true` when it had to choose between declarations). Invariants:

- **Never describe an answer as better than it is.** A call that landed on a
  variable, parameter or field is `value`, not `definition`; `ResolverMetrics.resolved`
  means "the backend answered", and `to_value`/`unlocated` count the cases it
  answered with something that is not a callee or not a node.
- **`flow` is written only by `src/flow.rs`**, after the resolution loop, and only
  for a `value` call. Visitors feed it: an `alias` occurrence (the right-hand side
  of `x = y`, *enclosed by the variable*, `detail` `return`/`result` for a returned
  name or a call's result) and a `read` occurrence whose `detail` is
  `arg:LINE:COL:INDEX` (or `...:=NAME`) when it is a call argument. The loop of
  every adapter that has them (`python/adapter.rs`, which TypeScript also uses,
  `golang`, `rustlang`) builds a `Flow`, hands it the bound targets and calls
  `finish`. It is flow- and context-insensitive and says so with the label.
- **A decorator is called by the scope the definition sits in**, not by the
  definition; the definition only `REFERENCES` it. Calls in a closure belong to the
  function that contains it. Both conventions are what the accuracy suites assume.
- **A Python keyword argument's name is a `read` occurrence with detail `kw`**, and
  the adapter keeps its REFERENCES edge only when the answer binds to a field
  (`ATTRIBUTE`/`VARIABLE`: dataclass, NamedTuple, TypedDict, `dataclass_transform`
  models such as pydantic). An answer on a parameter, a function, an external, or
  no answer at all is dropped and counted `unresolved` (like an import that reaches
  nothing) - never turned into an `EXTERNAL_SYMBOL`. `kw` occurrences never feed
  the flow pass.
- **A TypeScript object-literal key or method name is a `read` occurrence with detail
  `key`**, filtered like `kw`: the adapter keeps the REFERENCES edge only when the
  answer binds to an `ATTRIBUTE`, `VARIABLE` or `METHOD` of the project, and drops it
  (counted `unresolved`, never an `EXTERNAL_SYMBOL`) otherwise. A `key` or `member`
  (property write, `obj.x = v`) occurrence whose answer is its own site is a
  declaration of the property and produces no edge. Member reads are plain `read`s
  at the property; a callee's name stays a `call` only.
- **A local is a node only when it is called** (Go, Rust): nothing else needs one,
  and a node per local would multiply the graph.
- **ty's project is the analysed root**, never an ancestor's `pyproject.toml`
  (`ty_embedded.rs::prepare`): resolution that depends on what surrounds the
  directory is not a property of the directory.
- **`declared_at`** on a node lists extra name spans the type checker may answer
  with (overload signatures, a function-valued field); `SpanIndex::from_graph`
  reads it.
- **A resolver answer at 1:1 of a project file with no name there binds to that
  file's MODULE** (`SpanIndex::lookup_module`, tried after `lookup_name`; Python and
  TypeScript only, the other adapters do not call it). A real name at 1:1 wins: the
  placed MODULE is kept out of the name table, because a zero-width span would beat
  `x = 1` on the smallest-area tie-break. Module-name segments of an internal import
  statement are `read` occurrences (not `import`, which would make a RESOLVES_TO from
  the import to the package); a call that lands on a MODULE is still `unlocated`.

`callix analyze` prints where each language's calls went, and
`callix report -t status` counts the edges by `resolution`; the per-language
"known limits" there (`crates/callix-export/src/report.rs::language_resolution`)
are claims — change them when a limit is lifted, and when one is found.

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

## The command line and the images

A Cargo workspace beside the binding (the root package stays `callix`, so maturin,
`stub_gen` and the wheel matrix are untouched):

- **`crates/callix-export`** reads the canonical graph JSON into its own model —
  no pyo3, no Python — and writes LSIF, SCIP, CycloneDX, CSV, DOT, Mermaid and the
  text/Markdown/JSON reports. `index.rs` reads the graph as an index (documents,
  placed definitions and references, symbols) once for both LSIF and SCIP; `ops.rs`
  writes the model back as `json.dumps` would and does merge and diff. The index
  places what `Relation::is_placed_reference` accepts: the four reference kinds plus
  a RESOLVES_TO that records `metadata["span"]` (the name an import brings in); a
  structural RESOLVES_TO is neither placed nor counted, and
  `RelationKind::is_reference` (reports, counts) stays the four kinds. The SCIP
  encoder is hand-written protobuf, cross-checked against a rust-analyzer index. Each exporter is `fn(&Graph, &mut dyn Write,
  &Options) -> Result<Vec<Warning>>`: it streams, and it reports what the format
  could not carry instead of silently dropping it. `lsif::check::validate` is the
  structural checker the tests use.
- **`crates/callix-cli`** is the `callix` binary. It depends on `callix` with the
  default features, i.e. **without** `python`: the native engine, with nothing
  linked to libpython (`cargo tree -p callix-cli` has no pyo3, `ldd` no python).
- **`crates/callix-compat`** is a proc-macro crate of no-op `pyclass`,
  `pymethods`, `pyfunction`, `pymodule` and `gen_stub_*` attributes.

**The crate has two builds from one source.** `--features python` (what maturin
selects in `pyproject.toml`) is the extension module. Without it, `src/py/native.rs`
stands in for the parts of PyO3 the adapters touch — `Python`, `Py`, `Bound`,
`PyDict`, `PyList`, `PyErr`, `Value` — as plain Rust, and `callix-compat` turns the
attributes into no-ops, so a `#[pymethods]` block is an ordinary `impl` whose
methods the native code calls. `crate::py` re-exports PyO3 or the stand-ins;
every file imports from there, never from `pyo3` directly. Rules that follow:

- **Native is the default** (`default = []`), because cargo unifies features across
  workspace members: with `python` on by default, `cargo build --workspace` would
  build the CLI against the Python build. Anything Python needs says
  `--features python` (`task stubs`, `task test:rust`, `task lint` do).
- Code that talks to the interpreter — `serde.rs`, `serialization.rs`,
  `diffing.rs`, the `custom_*` helpers in `resolver_slot.rs`, `custom_declared`,
  `run_extractors`, the low-level `#[pyfunction]`s that take a `Bound<Graph>` — is
  `#[cfg(feature = "python")]`, with an error-returning stub where the native
  build still calls it. Custom resolvers, dependency parsers and boundary
  extractors are Python-only by nature.
- `src/native.rs` (native build only) is the engine's front door: `detect`,
  `analyze_tree` (run each language, merge with `allow_shared`, sum the resolver
  counters). `src/json.rs` writes the graph with `json.dumps`' layout and Python's
  float repr; `tests/api/test_native_engine.py` requires the CLI's bytes to equal
  `Graph.to_json()` for every fixture, so a divergence in either engine is caught.
- `PythonAdapter` classifies imports with `sys.stdlib_module_names` in the Python
  build and with the list in `src/python/stdlib.rs` (3.13) in the native one; ty
  finds the project's packages through a `python3` on PATH or a `.venv`.
- Both builds must stay clippy-clean: `task lint` runs the workspace (native) and
  `-p callix --features python`. `dead_code` and `unused_imports` are allowed in
  the native build, where the Python-facing layer has no caller.

Things that are easy to get wrong:

- **`tests/fixtures/rust` is `exclude`d from the workspace.** Without it cargo
  finds the repo's workspace from inside the fixture, refuses it, and
  rust-analyzer resolves nothing — as a quiet `unavailable`, which the golden run
  reports as a mismatch only when rust-analyzer is installed.
- **Language detection uses `collect_files`, not `can_handle`.** `can_handle` only
  looks at the root itself, so a Go module in a subdirectory fails it while the
  sources are found.
- **LSIF needs positions, and every resolved edge has them.** Each adapter records
  `metadata["span"]` (and `access` on a `REFERENCES`) on the edges it builds from a
  use-site; `tests/api/test_edge_spans.py` pins it. The C family once pushed those
  edges bare, which also let `dedupe_structural_relations` fold two call sites into
  one — an edge without metadata is "structural" to it.
- **An edge's position belongs to `metadata["file"]` when it has one, else to its
  source node's file.** Definitions with one qualified name are one node in the
  first file that declared it (Go `init`, C++ overloads), so an edge from it may
  have been written elsewhere; `Graph::note_site_file` records the file then and
  only then. Placing a reference by the source node's file alone put eight gin
  references in an eleven-line file.
- **Decorators, parameter defaults and a call's receiver are values the definition
  depends on, and `scan_value` must reach them.** The Python and TypeScript
  visitors once skipped all three — a bare `@deco` recorded nothing, a default was
  never visited, and `a.b(c).d()` kept only `d` — so a route handler had no edge
  to its router or its `Depends(...)`. The enclosing node is the decorated
  definition. `tests/api/test_decorators_and_receivers.py` pins each form.
- **A boundary's position must lie inside its handler's span, and its file must be
  keyed like the nodes.** `apply_boundaries` finds the handler by comparing the
  position with function spans under `node.file_path` (relative). A decorator or
  attribute sits *before* the function, outside its span, so the extractors hand
  over the function's name instead; Python and TypeScript once keyed the file by
  its absolute path, which matched nothing and filed every route under its file.
- **Documentation is `metadata["doc"]`, read in one place per language.** The
  comment block directly above a definition (`src/doc.rs::leading_comment`; no
  blank line, attributes and decorators allowed between) and, for a Python
  function or class, its docstring first. The Python/TypeScript/PHP `make_node`,
  the Go/Rust `declare` and, for C and C++, the `DeclLedger` (the definition's
  comment, else the first declaration's, since headers hold it) call it. The
  baseline fingerprint hashes kind, name and span, not metadata, so documenting
  a node moves no pin; the goldens do change.
- **An import is bound to a MODULE by name, not by what the walk has met.** The
  Python visitor is given every module of the root up front
  (`with_project_modules`): a module's id is a function of its name, so an import
  of a module whose file comes later in the walk still lands on it. It once fell
  back to an external symbol, or to the enclosing package, depending on file
  order. A `from m import name` also records an `import` occurrence, so the
  resolver adds a `RESOLVES_TO` from the import to the definition.
  In TypeScript an internal import specifier records an `import` occurrence at its
  ORIGINAL name (never the alias; the default and namespace identifiers too) and the
  IMPORT's `name_span` is the local binding; the module string of an internal import
  or re-export statement with a RELATIVE path is a `read` occurrence (the whole literal,
  enclosed by the container), so it REFERENCES the MODULE (a path-alias string such as
  `"@/store"` is not recorded: the resolver does not answer its file position, and the
  read would become a site-keyed external; its specifiers are still placed); a local
  `export { x }` and the specifiers
  of a re-export are `read` occurrences of their source name; `export * from` and
  `export * as N from` make the same `__reexport_` IMPORT as `export {..} from`.
  External specifiers and module strings get nothing. IMPORTS stays unpositioned so
  it keeps being deduplicated.
- **An import statement is an import wherever it is written.** `walk_statement`
  hands `import`/`from ... import` to `visit`, so an import in a function body or
  inside a module-level `if TYPE_CHECKING:`/`try:` is an IMPORT node
  `{scope}.{local}` declared by its container, with the same IMPORTS/RESOLVES_TO
  edges and an `import` occurrence on the imported name only. They once fell to
  `scan_value`, which recorded every name, the alias included, as a `read` and
  left no IMPORT node or IMPORTS edge.
- **Every name in a Python annotation is an `annotation` occurrence, not just the
  first.** `src/python/annotation.rs::type_names` walks subscript arguments, `|`,
  `Callable` lists, a quoted forward reference (re-parsed, with positions offset
  into the string) and annotated assignments (module variable, class field, local,
  `self.x: T`). HAS_TYPE runs from the VARIABLE, ATTRIBUTE or PARAMETER (or the
  function, for a return type). In `a.B` the type is `B` and `a` is a `read`.
  `Literal[...]` arguments and `Annotated` metadata are not types. An
  annotation-only statement has no right-hand side: take
  `child_by_field_name("right")`, never the last child, or the type is scanned
  as a value.
- **Every name in a TypeScript type is an occurrence, not just the head.**
  `src/typescript/type_uses.rs::type_uses` is a pure walker over a type subtree
  (unions, generics, `a.T`, `typeof`, indexed access, mapped, conditional, function and
  object types). HAS_TYPE (`annotation`) in declaration positions: variable, field and
  return annotations, parameters, alias bodies, and type-parameter constraints and
  defaults. `read` (REFERENCES) in expression and heritage positions: type arguments of
  calls and `new`, `as`/`satisfies`/`<T>x`, `f<T>`, and the type arguments and namespace
  prefixes of an `extends` clause. In `a.T` the type is `T` and `a` is a `read`; a
  `typeof v` operand is a `read`. A type parameter, a mapped-type key and an `infer`
  name are never recorded (a type parameter is no node, so its edge could only be a
  per-site `unlocated` external symbol), nor are parameter names inside a function type.
  `scan_value` has explicit arms for `as_expression`, `satisfies_expression`,
  `type_assertion` and `instantiation_expression`: without them the default branch walks
  the type as a value. `heritage_base_nodes` takes the `name` of a
  `nested_type_identifier`, so `interface J extends a.Shape` inherits. Not walked:
  `implements` clauses, annotations of inline closure parameters, JSDoc types.
  `tests/api/test_type_positions.py` pins each form.
- **An `EXTERNAL_SYMBOL` keyed `call@path:line:col` names a site, not a symbol.**
  The LSIF exporter reads the identifier from the source for its hover and gives
  it no moniker.
- **`--strict` always fails for PHP, C and C++**, by the same design that makes
  them `degraded`.
- **An assignment target is declared where Python binds it.** `self.x` / `cls.x`
  is ONE `ATTRIBUTE` of the nearest enclosing class (`Class.x`, not
  `Class.method.x`), declared by the class, with every write site in `declared_at`
  so the resolver's answer at any of them lands on the node. `obj.x = v` declares
  nothing: it reads the receiver and records a `write` of `x`, which the resolver
  binds to the real attribute (it has no holder, so the flow pass does not follow
  the value). Every target of `a = b = v` is declared and `v` is scanned once.
  Module-level `for` and `except ... as` targets are module `VARIABLE`s (declared
  on their own span, never the whole statement); a function's loop variable stays
  a node only when it is called.
- **A TypeScript initializer, pair value or assignment side is read from its grammar field** (`value`, `right`, `body`), never as "the first named child that is not X": tree-sitter makes a comment a named extra, so `const x = /*@__PURE__*/ f()` once lost every edge in the initializer. A method or accessor of an object literal reached by `scan_value` (an argument, a returned or nested literal) is an inline closure of the containing scope; only class-body methods are a scope boundary there.

The Docker images (`Dockerfile`, `docker/smoke.sh`) are plain Debian: `slim`
(callix) and `full` (+ Go, Rust with rust-analyzer and rust-src, Node, Python+pip).
`release.yml` also attaches `callix-<version>-<os>-<arch>.tar.gz` to the GitHub
Release (Linux glibc, macOS). They bring the
tools, not the analysed project's dependencies — `--prepare` fetches those for Go,
Cargo and npm; Python's are installed by the user into the container's Python.
`.github/workflows/docker.yml` builds and smoke-tests on PRs; `release.yml` pushes
per-architecture digests to ghcr.io and stitches the multi-arch manifest.

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
