---
sidebar_position: 4
---

# Thread safety

callix is single-threaded by construction. Nothing in `src/` releases the GIL —
there is no `allow_threads` call anywhere in the crate — and six of its classes
refuse to leave the thread that created them. Threads therefore buy a caller
nothing, and sharing the wrong object between them fails in ways that are worth
recognising before they happen.

Everything below was checked against the built module rather than reasoned from
the pyo3 documentation.

## The short version

- **One adapter per thread.** Never share one, and drop it on the thread that
  built it.
- **`analyze()` holds the GIL from the first file to the last edge.** Running
  two in threads takes as long as running them one after the other.
- **For parallelism, use processes** and merge the graphs afterwards.
- `Node`, `Relation`, `Span` and the kind enums are immutable and safe to pass
  anywhere.
- `Graph` is mutable and unsynchronised. Sharing one is memory-safe but costs
  determinism, and can raise.

## Sharing guarantees, class by class

The third column answers one question only: may an object of this class be
touched from a thread other than the one that created it?

| Class | Declared as | Cross-thread | |
|---|---|---|---|
| `NodeKind`, `RelationKind`, `ResolverStatus` | `frozen, eq, eq_int, hash` | yes | immutable enums |
| `Span` | `frozen, eq, hash, get_all` | yes | a copied value type |
| `Node`, `Relation` | `frozen, get_all` | yes | but `.metadata` is an ordinary Python dict, shared and mutable |
| `OccurrenceRef`, `ResolvedRef`, `BoundaryRef` | `frozen, get_all` | yes | |
| `ImportClassifier`, `TsImportClassifier` | `frozen` | yes | |
| `GraphDiff` | `frozen, get_all` | yes | holds `Py<Node>` / `Py<Relation>` handles, so reading it needs the GIL — which Python code always has |
| `ResolverMetrics` | `get_all, set_all` | movable | mutable and unsynchronised |
| `Graph` | — | movable | see [Graph](#graph) below |
| `SpanIndex` | — | movable | every lookup mutates it: the per-file span lists are sorted lazily |
| `RustAdapter`, `YamlAdapter`, `PhpAdapter`, `CAdapter`, `CppAdapter` | — | movable | but sharing one still breaks; see [the borrow rule](#the-borrow-rule) |
| `RustResolver`, `PhpResolver`, `CFamilyResolver` | — | movable | none of them holds a session or a database |
| `PythonAdapter`, `TypeScriptAdapter`, `GoAdapter` | `unsendable` | **no** | |
| `EmbeddedTyResolver`, `TsResolver`, `GoResolver` | `unsendable` | **no** | |

`frozen` here means what it means in pyo3: the Rust struct has no interior
mutability and Python cannot assign to its fields. `node.name = "x"` raises
`AttributeError: attribute 'name' of 'callix._core.Node' objects is not
writable`. The `metadata` dict hanging off a node is not covered by that — it is
a plain `dict`, with plain `dict` semantics.

That five of the eight adapters are not `unsendable` is an artefact of what their
resolvers hold — a salsa database, a Go session handle — not a guarantee about
the adapter. Treat all eight the same: one per thread, dropped on the thread that
built it.

## `unsendable` is a panic, not an exception

An `unsendable` class carries the id of the thread that built it and asserts on
it at every access. Violating that does not raise a tidy error:

```
thread '<unnamed>' panicked at pyo3-0.29.0/src/impl_/pyclass.rs:1068:9:
assertion `left == right` failed:
  callix::python::adapter::PythonAdapter is unsendable, but sent to another thread
  left: ThreadId(2)
 right: ThreadId(1)
```

The Rust panic is printed to stderr and converted to `pyo3_runtime.PanicException`,
whose bases are `BaseException` and `object` — so `except Exception` does not
catch it.

Deallocation counts as access. An adapter built inside a worker thread and
released on the main thread produces, at the `del`, a second complaint:

```
RuntimeError: callix::python::adapter::PythonAdapter is unsendable,
              but is being dropped on another thread
```

That one arrives as an unraisable exception during `tp_dealloc`: it is printed
with a traceback, execution continues, and the object is not freed. Keep the
reference inside the thread that made it.

## Graph

`Graph` holds an `IndexMap<String, Py<Node>>`, a `Vec<Py<Relation>>`, a
`Py<PyDict>` of metadata and two lazily built edge indices
(`src/graph.rs`). It is a non-`frozen` pyclass, so pyo3 guards it with a
GIL-checked borrow flag. Two threads can never corrupt it. Three things still go
wrong.

**Insertion order stops being deterministic.** Node order is part of the output
contract — that is why the store is an `IndexMap` and not a `HashMap`. Two
threads filling one graph interleave at whatever bytecode boundary the
interpreter chooses. Five runs of the same two-thread script, 3,000 nodes each,
put the first node of the second thread at index 0 every time and the switch
points somewhere different every time. The graph is correct; its serialization
is no longer reproducible, and `diff()` against it is no longer meaningful.

**Queries take an exclusive borrow.** `outgoing`, `incoming`, `callers`,
`callees` and `link_boundaries` all take `&mut self`, because the incoming and
outgoing edge indices are built on first use and dropped whenever an edge is
added. Nothing about them is read-only.

**`RuntimeError: Already borrowed` is reachable.** The borrow is held for the
whole Rust call, and a call that runs Python bytecode can be preempted. The
shortest real example is `to_dict()`: metadata values of unknown type are
coerced through `str()`, which is a Python-level `__str__`. With one thread
serialising such a graph and another calling `add_node`, the mutator fails:

```python
# thread A
graph.to_dict()          # holds a shared borrow across __str__
# thread B
graph.add_node(node)     # RuntimeError: Already borrowed
```

**`graph.nodes` and `graph.relations` are snapshots.** Each access builds a
fresh container, so iterating one is safe but costs O(n) and will not show
another thread's later additions.

## The borrow rule

Every `analyze()` takes `&mut self` on the adapter and holds it for the whole
call. Any adapter that is reachable from a second thread, and that runs Python
code during the call, will therefore fail:

```python
adapter = YamlAdapter(boundary_extractors=[SlowExtractor()])
# four threads calling adapter.analyze(root)
# → RuntimeError: Already borrowed  ×3
```

Custom resolvers, custom dependency parsers and custom boundary extractors are
all called through the ordinary Python protocol from inside that borrow, which
is what opens the window. The three `unsendable` adapters cannot reach this
state, because the thread check fires first.

## analyze() holds the GIL

Nothing in the crate calls `Python::allow_threads`, so the interpreter stays
locked for the duration of an analysis. Measured on a 12-core Linux box,
analysing a Python project of about 28,000 graph nodes four times:

| | wall clock | |
|---|--:|---|
| four analyses, one after another | 5.76s | |
| four analyses, four threads, one adapter each | 5.95s | 0.97× |
| four analyses, four processes | 1.74s | 3.30× |

The structural pass alone behaves identically: 1.80s sequential against 1.85s
threaded. It is not that the threads contend — they never run.

Two consequences follow, and both are visible from the outside.

A background thread stops running. A ticker thread sleeping 1ms in a loop got
**two** ticks during a 1.50s analysis, with a maximum gap of 1,466ms against a
1.3ms baseline.

`Ctrl-C` is deferred. Python signal handlers only run when the main thread
executes bytecode, and the main thread is inside Rust. A `SIGINT` sent 0.30s
into an analysis raised `KeyboardInterrupt` at 1.52s — when `analyze()`
returned.

For Rust this is at its most pronounced. `RustResolver::prepare` spawns
`rust-analyzer scip` and waits for it in a `std::thread::sleep` poll loop
(`src/rustlang/resolver.rs`) without releasing the GIL, so the whole interpreter
is frozen for the entire index build — around 96 seconds of the ~100 an analysis
of ruff takes, per [Benchmarks](./benchmarks.md). A 2.09s analysis of a
four-file crate fixture let the ticker thread run **once**.

## Why the parsers are thread-local

Each adapter keeps its tree-sitter parser in a `thread_local!` holding a
`RefCell<Parser>` — one per language module, and two in `src/cfamily/mod.rs`,
since C and C++ are separate grammars behind one implementation.

What that buys: a `tree_sitter::Parser` is not `Sync` and cannot be shared, and
loading a grammar costs noticeably more than parsing a file. A thread-local
gives every thread its own parser with no global lock and pays the grammar setup
once per thread. It also means a future `allow_threads` would not have to
restructure parsing.

What it does not buy: parallelism. Today only one thread ever holds the GIL, so
only one parser is ever in use.

The genuinely shared parsing state is the compiled boundary queries, cached in
`OnceLock`s per process — every boundary extractor does this. TypeScript's cache
is a `OnceLock<Mutex<HashMap<…>>>` because its query text varies with `.ts`
versus `.tsx`; the mutex protects the cache, not a parser.

## Resolvers

| Resolver | State it holds | Shareable | Re-entrant on one thread |
|---|---|---|---|
| `EmbeddedTyResolver` | a salsa `ProjectDatabase` | no — `unsendable` | yes: `resolve_all` takes `&self` |
| `TsResolver` | an `int` handle into a Go-side session map | no — `unsendable` | yes |
| `GoResolver` | the same, plus a detected `GOROOT` | no — `unsendable` | yes |
| `RustResolver` | decoded SCIP tables, plain data | movable | yes |
| `PhpResolver` | an autoload map and symbol tables, plain data | movable | yes |
| `CFamilyResolver` | nothing — it answers `status()` and the table lives inside `analyze` | movable | yes |

The salsa database behind ty is not `Sync`, which is the reason for the
`unsendable` marker on `EmbeddedTyResolver` — the comment above the attribute
says so. The Go and TypeScript resolvers keep nothing but a numeric handle; the
session itself lives in the Go runtime, in a map guarded by a `sync.Mutex`
(`go/bridge.go`, `go/bridge_go.go`). Several sessions coexist in one process
happily; what serialises access to a single one is the GIL, not the bridge.

`RustResolver` ends up sendable because after `prepare` it is only tables. It is
still one-analysis-at-a-time by construction: the SCIP index is written to
`$TMPDIR/callix-{pid}.scip`, keyed on the process rather than the thread.

Every adapter calls `prepare` on its resolver at the start of every
`analyze()`, replacing whatever the previous call left behind. An adapter is a
stateful object for exactly one analysis at a time; reusing one serially is
fine, nesting or overlapping two is not.

## What to do instead

### Processes

Fan out per language or per root, serialize, merge in the parent. Merging is
the point at which a polyglot repository becomes one graph anyway.

```python
from concurrent.futures import ProcessPoolExecutor

from callix import Graph, GoAdapter, PythonAdapter

ADAPTERS = {"python": PythonAdapter, "go": GoAdapter}


def analyse(job):
    language, root = job
    return ADAPTERS[language]().analyze(root).to_json()


if __name__ == "__main__":
    jobs = [("python", root), ("go", root)]
    with ProcessPoolExecutor() as pool:
        parts = list(pool.map(analyse, jobs))

    graph = Graph.from_json(parts[0])
    for text in parts[1:]:
        graph.merge(Graph.from_json(text), allow_shared=True)
    graph.link_boundaries()
```

JSON is the transport because it is the format both sides already agree on;
pickling a `Graph` is not supported. `allow_shared=True` is needed because two
adapters deliberately produce the same `BOUNDARY` node — see
[Cross-language analysis](../guides/cross-language.md).

The cost is memory: each worker loads its own ty database or Go session, and
those are the large part of the footprint reported in
[Benchmarks](./benchmarks.md). Size the pool against RAM, not against cores.

### If you use threads anyway

Build the adapter inside the thread, use it there, and let it go there. A graph
handed back to the parent afterwards is fine — `Graph`, `Node` and `Relation`
carry no thread affinity.

```python
def work(root, out):
    out.append(PythonAdapter().analyze(root))   # adapter never leaves
```

This is correct, and it is not faster. The only thing it wins is code shape.

### Free-threaded CPython

Not supported. The extension is built against the limited API with
`abi3-py310`, so pyo3's build script emits only `Py_3_10` and `Py_LIMITED_API`
and the `Py_mod_gil` module slot — the one that would declare free-threading
support — is compiled out entirely. A free-threaded interpreter has to assume
the module needs the GIL, which is exactly right.
