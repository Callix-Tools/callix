# Runtime call graphs

The fourth reference callix is measured against. The accuracy suites hold
hand-written call graphs, `tests/baseline` holds fingerprints, and the SCIP
comparison holds what static indexers say. None of them shows what a program
REALLY calls at run time: dynamic dispatch through interfaces, traits and
virtual methods, callbacks, higher-order flow. This directory holds call graphs
recorded while a real project ran its own tests or a fixed workload, and
`runtime.py` says which share of the executed calls callix knows.

It is a **recall target**, not a precision measure. A trace is a lower bound:
an edge that did not execute proves nothing, so callix having an edge the run
never took is not an error. Precision stays with `tests/accuracy` and SCIP.

## The artifact

One file per project, `tests/runtime/<owner>__<repo>.json`, compact and
deterministic (header keys in a fixed order, one function and one edge per
line). Only calls whose BOTH ends are functions of the project itself are kept.

```json
{
"schema": 1,
"project": "redis/redis",
"ref": "8.10.0",
"sha": "<commit>",
"language": "c",
"tool": "valgrind callgrind-3.22.0",
"workload": "<one line: what was run>",
"command": "<exact command>",
"functions": [
  {"id": 0, "file": "src/server.c", "name": "processCommand", "line": 3912}
],
"edges": [
  [0, 7, 124]
]
}
```

- `file` is relative to the project root (posix). `line` is the first line of
  the function's definition when the tool knows it, else 0. `name` is as the
  tool printed it (C++ demangled, Rust path, Go `pkg.(*T).M`, Python `mod.Class.m`).
- `edges` are `[caller, callee, call_count]`, sorted and unique.
- `sha` is the checkout the lines belong to; `compare` refuses a checkout at a
  different commit, because lines would shift.
- No raw traces in git. `capture/common.py` writes and validates the format and
  is shared by every capture script; `capture/callgrind.py` turns callgrind
  output into an artifact (C, C++ and Rust).

```bash
python tests/runtime/capture/callgrind.py callgrind.out.* \
    --root /path/to/checkout --project owner/repo --ref v1.2.3 --language c \
    --workload "what ran" --command "valgrind --tool=callgrind ..." \
    -o tests/runtime/owner__repo.json
```

## Capture rules

- Build WITHOUT inlining so callers and callees stay separate functions: C and
  C++ `-O0 -g -fno-inline`, Rust debug with `opt-level = 0`, Go
  `-gcflags=all=-N -l`.
- Only in-project ends: dependencies, vendored code and the standard library
  are dropped (`deps/`, `vendor/`, `third_party/`, `node_modules/`, `target/`).
  The project's own tests count as project code.
- A function's `line` is the smallest line the tool attributed to it inside its
  own file.

## How the comparison works

`task runtime` analyses the pinned checkout with the `callix` binary, then:

1. **Mapping.** Each artifact function goes to a callix node (FUNCTION or
   METHOD) by file plus the last name component (C and C++ headers match
   through `declared_at`). Several candidates are narrowed by the definition
   whose span contains `line`, then by the qualifier (`T` in `T::m`). An
   anonymous function (closure, lambda, Go `funcN`) maps to the callable or
   function-valued variable whose span contains its line, matching callix's
   convention that a closure's calls belong to its enclosing function. There is
   no line-only fallback for named functions, so naming-scheme mismatches show
   up instead of being hidden.
2. **Scoring.** Edges are deduplicated after mapping (template and generic
   instances count once, call counts are summed), so recall counts node pairs,
   not raw edges. Each pair is exactly one of:

| Category | Meaning |
|---|---|
| `implicit` | the callee is called by the language, not written at a call site (see below). Checked first; never a miss. |
| `unmapped` | an end has no callix node. Reasons: `no_file` (the graph has no node in that file), `no_name`, `ambiguous`. |
| `folded` | both ends map to one node but are different raw functions (a closure folded into its parent). Reported, not scored. |
| `found` | callix has a CALLS edge between the two nodes, whatever its `resolution` (including `flow`). Broken down by resolution. |
| `wrong_target` | the caller has a CALLS edge to another node of the same name (an interface or virtual declaration, a wrong overload, an EXTERNAL_SYMBOL). Name based, because artifacts carry no call sites, so it can over-count when callix bound a different same-named callee for another site. |
| `missing` | anything else. |

`recall = found / (found + wrong_target + missing)`, rounded to 4 places
(`recall_by_calls` weights the same split by call count). An unmapped share
above roughly 20% means the naming scheme needs work before the recall means
anything.

### Implicit calls

Reported in their own category: C++ constructors, destructors and `operator...`;
Rust `drop`/`drop_in_place` and methods of std traits (`core::ops`, `core::iter`,
`core::fmt`, `core::cmp`, `core::hash`); Python `__dunder__` methods; PHP
`__construct`, `__destruct` and other magic methods; TypeScript `constructor`
and getters/setters; Go `init`. C has none. The per-language capture issues
extend `is_implicit` in `runtime.py` (and this list) when a capture shows more.

## Tasks

```bash
task runtime              # compare (clones what is missing) + check against thresholds.json
task runtime:update       # compare, then raise the floors; never lowers them
task runtime -- --project redis/redis -v
```

`thresholds.json` is a ratchet like `tests/accuracy/thresholds.json`: `check`
fails below the floor, `update` only raises it. `report.json` is the run output
of the last `compare` (it depends on the machine and toolchain, so it is
ignored by git). With no artifact the commands print `no runtime artifacts
yet` and succeed.

Not part of `task test` or CI: it needs clones and, for a new capture, valgrind
or an equivalent. The unit tests (`tests/unit/test_runtime_compare.py`,
`test_runtime_capture.py`) run on synthetic data and are in CI.

## Recorded projects

| Project | Language | Tool | Workload | Recall |
|---|---|---|---|---|
| _none yet; the per-language capture issues add rows_ | | | | |
