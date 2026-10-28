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
decide its span. The ledger keeps (file, name_span) per site, which is what
`declared_at` is made of. An `#include` path literal (delimiters included) is
the IMPORT's `name_span` and the span of its RESOLVES_TO; the FILE-to-header
IMPORTS edge stays unpositioned. An exporter defines a FILE on its first
character (1:1-1:2, never an empty range: `docker/check_lsif.py` rejects one) only
when a placed edge targets it. The same goes for any definition whose name span is empty
(a Python or TypeScript MODULE is placed at a zero-width 1:1): the exporters widen it
to one character, the graph keeps the zero width. With the sources at hand, LSIF moves a definition
that still covers no text (a file that starts with a blank line) to the file's first
non-blank character, and gives an empty file's definition no range at all.

**Before the scope pass, the adapter blanks the project's erasable macros**
(`src/cfamily/erase.rs`: bodies that are empty, an attribute, a calling convention,
`inline`, `extern "C"` or a storage keyword that fits the macro's name, in every `#if`
arm) with same-length spaces, so `class API Foo {}`, `int x GUARDED_BY(m);` and
`API int (*f)(int);` parse as written and every span stays a position in the original
file. Header ownership (`claims`) is decided on the RAW bytes, because the blanked text
of a C++ class can parse cleanly as K&R C; user boundary extractors get the raw source.
The probe rule: a `.h` goes to C++ when its clean C parse has a K&R definition without a
function declarator (`namespace a {`, `class C {`) or a C++ keyword as a type name, and
also when no C root reads it (a tree with no `.c`, or a header outside every C root).
Both adapters decide the latter from the same discovery (`detector::family_root_files`,
`adapter::CReach`), so ownership stays one-adapter.
A header the OTHER dialect claims still contributes its macros to the erasable set,
though never to expansion: a header of nothing but `#define`s parses cleanly as C, so
leveldb's `export.h` belongs to C (in a tree with a C source: leveldb's `c_test.c`)
while `class LEVELDB_EXPORT DB` is C++. Ownership is unchanged (no FILE node, no
facts from that header). Macros defined outside the analysed root, or in another
root, are not erased (each root builds its own table).

**`src/yaml/` has no visitor and no resolver at all**: it
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
in `rustlang`; an inline `mod foo { ... }` is a MODULE too (without a `file_path`
or span, like the file modules). Rust dispatches `impl` blocks in a second pass
because a type may be declared below its `impl`. In `rustlang` an enum variant is a
CLASS `Enum.Variant` (called like a tuple struct), an associated type a TYPE_ALIAS and
an associated const a VARIABLE `Type.NAME`, an item in a function body is qualified
under the function (`m::f::Local`), and a const or static initializer's uses are
enclosed by the const. A Rust method is `{module}::{Type}.{method}` with no trait in
the name, so one node may stand for several impls, through `declared_at`.

In Go every name of a multi-name field (`a, b int`) is its own ATTRIBUTE, and the fields of an anonymous struct field type (also behind `*`, `[]`, `[N]`, a map value, a chan) are ATTRIBUTEs `T.F.<anon>.G`, DECLARED by the ATTRIBUTE `F`. There is no node for `<anon>`, and no `base`/`INHERITS_FROM` for types embedded in such a struct.

A Go MODULE is placed by `file_path` (its first non-`_test.go` file) plus
`metadata["name_span"]` (that file's `package` clause identifier) and has **no
`span`**, because a package spans several files: `SpanIndex::from_graph` skips
span-less nodes, so a package never steals a real name. The placement file is
chosen from the paths before the walk (`placement_files`).

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
- **Go package-level `var`/`const` initializers are scanned with the VARIABLE as
  the enclosing definition** (`GoExtractor::scan_initializer`). A func literal that
  is the whole value is the variable's body: its parameters are PARAMETERs
  `<var>.<p>` and its called locals are nodes under the variable. A literal nested
  deeper in the value is a closure and has no PARAMETERs. In `var a, b = f()` the
  first name owns the call, so no site gets two occurrences.
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
- **A Go bare identifier** (`limit` in `n := limit`, `counter = n + limit`, a return
  value, a case, a composite element, a range operand, a selector operand such as
  `quoteEscaper` in `quoteEscaper.Replace(s)`) is a `read`/`write` occurrence with
  detail `name:<ident>` (`GoExtractor::record_identifier`). `build_root_structure`
  keeps it only when `{package}.{name}` is a package-level VARIABLE or FUNCTION of
  the file's package and rewrites the detail to `name:<node id>`; `resolve_pass`
  writes the `REFERENCES` edge only when go/types binds the use to exactly that
  node. Locals, parameters, predeclared names and dot-imported names are dropped
  without an EXTERNAL_SYMBOL. One site, one edge occurrence: `GoExtractor::sites`
  holds the positions that already have a call/read/write.
- **A Go package qualifier** (`pkg.X` operand, `pkg.T` in a type) is a `read`
  occurrence with detail `package`. The pass binds it only when go/types answers
  with the declaring position of one of the file's import specs (the alias, else
  the path literal), so a local that shadows a package gets no edge. The edge is a
  `REFERENCES` to the MODULE, or to the import path's EXTERNAL_SYMBOL, classified
  with the import's own origin. The import path literal is the `span` of the
  structural `RESOLVES_TO`; `IMPORTS` stays unpositioned. Selector operands are
  filtered by the alias or the gopls assumed name (`assumed_package_name`), so an
  unaliased import whose package name differs from its last path element is missed
  in expressions; types are not filtered.
- **ty's project is the analysed root**, never an ancestor's `pyproject.toml`
  (`ty_embedded.rs::prepare`): resolution that depends on what surrounds the
  directory is not a property of the directory.
- **`declared_at`** on a node lists extra name spans. An entry is a bare `Span` in
  the node's own file (Python and TypeScript: positions the type checker may
  answer with - overload signatures, a function-valued field; Rust: every later
  declaration of a qualified name in the same file, such as same-named methods of
  different impls of one type (`impl Add<S>` / `impl Add<&S>`, an inherent and a
  trait method) and cfg variants; exporters do not place them), or `{file, span}` naming its file (C and C++: a prototype, forward
  declaration or in-class declaration is the SAME node as its definition, and the
  name span of every other declaring site is listed here, own-file prototypes
  included; exporters write each as a SCIP ForwardDefinition). `SpanIndex::from_graph`
  reads both shapes.
- **C/C++ type uses are `read` occurrences with `CallSite.type_use`**, giving a
  REFERENCES from the enclosing callable; a type in a declared position stays
  `annotation` (HAS_TYPE). They resolve only to CLASS/TYPE_ALIAS through
  `CFamilyIndex::resolve_type` (a written scope must match, no bare-name fallback).
  Scope components and `sizeof(name)` are `speculative` (a miss writes nothing, so a
  namespace makes no edge), and a local declaration's type yields to a construction
  at the same span. The role is `read`, not a new one, so custom resolvers still
  see it.
- **A C++ operator use is a speculative `call` occurrence named `operator<op>`**, at the
  operator token, with `CallSite.overloaded_operator` set (`=`, compound assignments,
  binary operators, `[]`; alternative tokens such as `and_eq` are normalised). The
  resolver (`resolve_operator`) never falls back to a bare name for it. `=`, `[]` and
  `->` are member-only; a free operator needs an operand's class among the classes its
  parameters name. Only fields and globals recorded as holding an object
  (`FoundDecl.holds_object`: no pointer, array or function declarator) can be the left
  operand, so `p_ = q` on a pointer field is not `operator=`. A miss writes nothing, so
  built-in arithmetic adds no externals. Operators on expression results, unary
  operators, ADL and templates are not followed.
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
  encoder is hand-written protobuf, cross-checked against a rust-analyzer index. A SCIP symbol's `<project>` is its node's own sub-root (`Symbol.project`, the PROJECT that reaches it through CONTAINS/DECLARES in `Index::build`), the first PROJECT only for a node none reaches; distinct definitions that still share a symbol (CLASS and TYPE_ALIAS are both `#`) put the non-primary kind under a `<kind>/` namespace (`scip.rs::disambiguate`). Each exporter is `fn(&Graph, &mut dyn Write,
  &Options) -> Result<Vec<Warning>>`: it streams, and it reports what the format
  could not carry instead of silently dropping it. `lsif::check::validate` is the
  structural checker the tests use. `index.rs` places an edge by
  `Relation::is_placed_reference()`: a `CALLS`/`REFERENCES`/`HAS_TYPE`/`INHERITS_FROM`, or a
  `RESOLVES_TO` that carries `metadata["span"]` (the name an import brings in). The
  structural, unspanned `RESOLVES_TO` of an import is not a reference, and a file that
  only re-exports still gets a document. `RelationKind::is_reference` is unchanged, for reports and counts.
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
- **Installed Composer packages are never PHP sub-roots.** `php_roots` drops a
  `composer.json` below a `vendor/` that is the first component of its path, or
  sits directly in a directory holding a `composer.json`. `PHP_EXCLUDED_AT_ROOT`
  in `collect_php_files` applies only relative to a sub-root, so a vendor package
  made a root of its own was walked in full and its namespaces became "internal"
  to the resolver.
- **A Rust annotation is every type name under the type**, the last segment of a path, once per site (`recorded` in `src/rustlang/visitor.rs`), and never the declaring name of a generic parameter. `Result<A, B>`, bounds, `where` clauses, `let`/alias/cast/closure types, tuple fields and enum payloads all count. An `impl` header's annotations, and the `base` of an `impl Trait for T` whose `T` is not a project class, are enclosed by the **FILE**, never the MODULE: a Rust MODULE has no `file_path`, so `note_site_file` records no `metadata["file"]` and `crates/callix-export/src/index.rs` drops an edge it cannot place.
- **A Rust path qualifier is a reference, never an annotation.** Every segment of a path before the last (`Foo` in `Foo::new()`, `inner`, `crate`, `super`, and every segment of `pub(in path)`, with the `crate`/`super` of `pub(crate)`/`pub(super)`) is a `read` occurrence, so the edge is a `REFERENCES` with `access: read`: a module is not a type and a `HAS_TYPE` to a MODULE would describe the answer as better than it is. It shares the one-site-one-occurrence set (`recorded`) with annotations. `self`, `<T as Trait>::` and primitive qualifiers (`u8::MAX`) are not recorded: no query that is certain to fail. A visibility is enclosed by the declaration, for a `mod` or a `use` by the FILE (a MODULE has no `file_path`). A module answer binds through `module_sites` in `src/rustlang/adapter.rs` (the start of a file, 1:1, maps to its file module; the name of an inline `mod` maps to its MODULE), because a MODULE has no span and the SpanIndex cannot see it.
- **A Rust name in expression or pattern position is a `read` with `detail: "value"`.** This covers a const or static read, the last segment of a path (`Mode::A`, `u32::MAX`), a struct literal's type and each field initializer or shorthand, the type path and fields of a struct or tuple-struct pattern, and an uppercase identifier in a pattern (`src/rustlang/visitor.rs::collect_values`). A lowercase pattern identifier is a binding and is not recorded. Callees, plain-name arguments and field accesses keep their own occurrence through the shared `recorded` set (`collect_calls` marks them). Names inside a macro the project defines, inside an opaque macro such as `matches!`, and the bare receiver of `x.f` are not recorded (the arguments of a std macro such as `println!` are blanked first and read as expressions; the blanking repeats until nothing changes, so `vec!(f())` inside `assert_eq!(..)` is reached). In `resolve_pass`, a value answer that lands on a local or a parameter (SCIP `local` symbol, which `symbol_to_ref` marks `ResolvedRef.kind == "local"`, or a PARAMETER or a function-local VARIABLE) is dropped: no edge and no EXTERNAL_SYMBOL. An enum variant stays an `unlocated` external until variants are nodes (RUST-06).
- **A Rust `macro_rules!` is a FUNCTION** named `module::name` (qualified under the function when written in a body) with `metadata["macro"] = "macro_rules"`; it gets a DECLARES edge from the FILE and, like every Rust function, no CONTAINS. Its invocation name is a `call` occurrence for any macro the crate defines (`RustExtractor::with_project_macros`, a crate-wide name set), on the last path segment of `crate::m!`. An item-level invocation (`name!(..);` outside any function) is a `call` occurrence enclosed by the FILE, never the MODULE (a Rust MODULE has no file_path), so its CALLS edge goes from the FILE to the macro's FUNCTION node; item-level invocations arrive as an `expression_statement` wrapping the `macro_invocation`, which `dispatch_items` unwraps. Invocations inside `impl` and `trait` bodies are not recorded. `resolve_pass` skips nodes carrying `metadata["macro"]` when it binds the `name:` callees written in a macro body, since such a token names a function. A fn and a macro with the same name in one module collapse into one node (known limit).
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
- **Go annotations cover every `type_identifier` of a type, root included.**
  `src/golang/visitor.rs::walk_type` seeds its stack with the node itself and
  reads in source order; `record_annotation` emits one `annotation` per named
  type (`map[K]V` depends on both) and skips predeclared names and type
  parameters in scope in the visitor, because neither is a node and a type
  parameter would become a per-site `annotation@` external. Interface method
  results are annotated too. `src/rustlang/visitor.rs::walk_type` had the same
  root-skip and includes its root too.
  Annotations are recorded at: receivers (enclosed by the method), the
  right-hand side and the constraints of type specs and aliases (enclosed by the
  TYPE_ALIAS or CLASS), `var`/`const` types (enclosed by the VARIABLE, one per
  name), and every `type_identifier` in a function body (enclosed by the
  callable, closures included; `collect_body_types`). Two exclusions: a type
  declared inside a function is not a node and would become an unlocated
  external, so its name is skipped (scope-insensitively); and `F[T](x)` parses
  as `type_conversion_expression{generic_type}`, so the base `F` would become a
  HAS_TYPE edge to a function — only its type arguments are annotated.
  A `call` occurrence that go/types binds to a CLASS/TYPE_ALIAS is a conversion
  `T(x)`: `resolve_pass` re-kinds it to REFERENCES (access read), never CALLS and
  never dropped.
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
  resolver adds a `RESOLVES_TO` from the import to the definition. A PHP `use`
  clause does the same at the last segment of the ORIGINAL name (never the
  alias): the symbol table answers it by fully-qualified name and use kind
  (class, function or const) without going through the file's alias map, a
  namespace alias (`use App\Contracts;`) binds to its MODULE, and the IMPORT
  node carries the clause's span.
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
- **A Rust `use` leaf resolves from its IMPORT, and `mod x;` is a reference from the
  FILE.** The IMPORT stays one node per declaration; the visitor reports one `UseSite`
  per name the tree brings in (`src/rustlang/visitor.rs`). The structure pass
  (`build_crate_structure`) binds each by path once every file of the crate is read
  (`mod util;` in lib.rs is met before util.rs): an `internal` leaf by its absolute path,
  an `unknown` one as a 2018 path relative to the importing module, and writes a
  `RESOLVES_TO` with `span` and `resolution: name` (a `mod x;` is a `REFERENCES` FILE ->
  MODULE with `access: read`). Only leaves it could not bind (std, dependencies, re-exports)
  go to the resolver as `import` occurrences, so no site is answered twice; an unbound
  answer with no name is dropped. `*`, `self`, `crate`, `super`, the `as` alias, a `use` inside a
  function body and a `#[path]` module are not sites. Edges into a MODULE are not exported
  (a Rust MODULE has no file).
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
- **A C-family member access carries its receiver, and the flat name table filters by kind.** `CallSite.member` / `receiver_path` (built by `Reader::member_site` in `visitor.rs`) say what `a->b->c` is read from; `CFamilyIndex::resolve` binds a call, a read, a write and a member initialiser by the receiver's declared type through field chains, typedefs (`aliases`) and file-scope variables (`value_type`), and says `Outside` when the type is known and lacks the field. Any by-name fallback goes through `lookup_kinds`/`by_bare_name` with a kind filter that runs BEFORE the first candidate is chosen: an unqualified name never binds to an `Attribute`, or a field declared first would hide a function of the same name.
- **A class, struct or union defined in a function body is a CLASS qualified by the enclosing callable's full name** (`run()::State`, members `run()::State::stats` and `run()::State::State()`; in C `run::State`). The function DECLARES it, its member bodies are callables of their own (their calls are not the enclosing function's: `body_nodes` in `visitor.rs` does not enter a record specifier that has a body), and the resolver binds it only lexically - from inside that callable, innermost scope first (`CFamilyIndex::local_class`, the `within` argument of `resolve`/`resolve_type`) - or by its exact name; it is never found by its bare name from elsewhere and is not a candidate of `visible()`. Split a member's class from its name with `headers::split_member` (parenthesis-depth aware), never with `split('(')`, and read parameters with `params_of`, which takes the LAST parenthesis group. A qualified record head `class Outer::Inner {` names `Inner` (name and name span); its qualified name is unchanged. A local enum or typedef is not declared.
- **`typedef struct { .. } T;` declares CLASS `T` beside TYPE_ALIAS `T`.** `anonymous_typedef_name` picks the name (the first plain `type_identifier` declarator, else the first named one) for `on_typedef` and for the scope pass; `on_record` receives it as a fallback, puts the name span on the alias identifier and takes span and doc from the `type_definition`, so the span contains the name span. An anonymous record's fields are hoisted into the enclosing record only when no typedef names it (C11 anonymous member). `resolver::type_key` keys the text before `{`, so a typedef of an inline tagged struct leads to its tag, while `struct { .. }` still names no class.
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
