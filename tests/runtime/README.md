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
  output into an artifact (C, C++ and Rust); `capture/xdebug.py` does the same
  for Xdebug profiles (PHP) and `capture/composer_git.py` installs a PHP
  project's composer packages from git.

```bash
python tests/runtime/capture/callgrind.py callgrind.out.* \
    --root /path/to/checkout --project owner/repo --ref v1.2.3 --language c \
    --workload "what ran" --command "valgrind --tool=callgrind ..." \
    -o tests/runtime/owner__repo.json
```

## Capture rules

- Build WITHOUT inlining so callers and callees stay separate functions: C and
  C++ `-O0 -g -fno-inline`, Rust debug with `opt-level = 0`, Go
  `-gcflags=all=-N -l`, PHP with opcache and JIT off.
- Only in-project ends: dependencies, vendored code and the standard library
  are dropped (`deps/`, `vendor/`, `third_party/`, `node_modules/`, `target/`).
  The project's own tests count as project code. A call made by non-project
  code into the project (a framework calling a handler, a test runner calling a
  test) therefore has no project caller and is not in the artifact.
- A function's `line` is the smallest line the tool attributed to it inside its
  own file.
- Every capture script builds and runs from a copy of the checkout at the
  artifact's `sha`, never in the analysed checkout itself.

Per tool:

- **callgrind** (C, C++, Rust; `capture/callgrind.py`). Keep the run
  single-process: a forked child (redis BGSAVE) writes its own `callgrind.out`
  that repeats every count the parent had before the fork (`capture/c.sh`
  asserts exactly one output file). A relative path counts only when it is a
  file under `--root` (glibc's debug info prints `./csu/./csu/init-first.c`),
  and a `calls=` target position does not move the call site, so a function's
  `line` stays right.
- **Rust** (`capture/rust.py`) works on a `git archive` copy where
  `#[inline(always)]` is rewritten to `#[inline]` (same line count), because
  LLVM's always-inliner runs even at opt-level 0 and would charge an inlined
  function's callees to its caller. Functions whose self type and trait are no
  workspace crate, and `#[derive]`-generated impls, are dropped.
- **Go** (`capture/go.py`, `capture/go/`) instruments a copy of the module with
  an entry hook per function. Calls from the standard library or a dependency
  into the project (net/http to a handler, `testing.tRunner`, `go f()`) have no
  project caller.
- **Python** (`capture/python.py`, `capture/python.sh`): a `sys.monitoring`
  PY_START tracer under CPython 3.12+. Only functions are recorded (code objects
  with `CO_OPTIMIZED`, so no module or class bodies); the caller is the frame
  below, looking through `contextlib` frames so `with cm():` is an edge from the
  caller. Decorator wrappers are kept as executed (caller -> wrapper ->
  decorated), the PyCG convention. Calls injected by tests (monkeypatch, fakes)
  stay in the artifact and are a recall ceiling.
- **PHP** (`capture/php.sh`, `capture/xdebug.py`): PHPUnit under the Xdebug
  profiler (an exact per-call trace), one PHP process per suite. A call through
  a builtin that takes a callable (`array_map`, `call_user_func`, `usort`,
  `Closure->call`, ...) is an edge from the builtin's caller to the callback,
  the convention of the accuracy suites; other builtins (`count`,
  `json_encode`, Reflection constructors, autoload) end the chain. Xdebug
  reports a `__call`/`__callStatic` frame under the called name, so the
  converter records it as the magic method. Closures are
  `Ns\Class->Ns\{closure:A-B}` and map to the enclosing callable by line.
- **TypeScript** (`capture/typescript.sh`, `capture/jsprobe/`): a vite plugin
  that instruments the sources with @babel/parser inside the project's own
  vitest run. Nothing is bundled or inlined, and every call between two project
  functions is recorded exactly, not sampled. Calls made while a module loads
  have no caller and are dropped; calls through code that is not instrumented
  (`Array.prototype.map`, vitest `expect`) are credited to the nearest project
  frame.

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
   up instead of being hidden. Per language:
   - **C++**: a gtest `Suite_Name_Test::TestBody` maps to the node of the
     `TEST`/`TEST_F`/`TEST_P` macro with that suite and name in the same file
     (exact match, no line fallback); `[abi:...]` tags are stripped; C++
     artifacts are analysed with `--lang cpp,c` (`ANALYZE_LANGS`), so a C test
     of a C++ project's C API maps.
   - **Rust**: a raw identifier (`async` in the symbol, `r#async` in the
     source) retries as `r#name`, and a function a project `macro_rules!`
     generates maps to the smallest macro FUNCTION (`metadata.macro`) whose span
     contains its line.
   - **Go**: callix keeps one `init` node per package, so every `init.N` of a
     directory maps to it; `pkg.init.funcN` (a closure in a package-level var
     initializer) maps by line to the variable.
   - **Python**: a lambda or generator expression maps by line to the smallest
     FUNCTION or METHOD containing it (callix makes a lambda a `<lambdaN>` node;
     a generator expression folds into its function, never into a variable),
     and `<locals>` components are dropped from qualified names.
   - **TypeScript**: the probe names functions as ECMAScript does, qualified by
     the enclosing named scopes (`f`, `Class.m`, `Class.get x`), and numbers
     anonymous ones in source order (`f.<anonymous>`, `<anonymous#2>`) and
     object-literal members (`f.<object>.key`, `<object#2>`), so siblings never
     merge. `split_name` drops the `<object>` part and a `get `/`set ` prefix
     (callix names a class getter METHOD `x`). An anonymous function or object
     member maps to the innermost FUNCTION/METHOD containing its line, else to
     the FILE node, never to a VARIABLE: callix writes no CALLS edge from a
     TypeScript VARIABLE and encloses top-level code (module initializers, test
     callbacks) by the FILE. A FILE caller is found when any top-level code of
     that file calls the callee, so recall is coarse for those callers.
2. **Scoring.** Edges are deduplicated after mapping (template and generic
   instances count once, call counts are summed), so recall counts node pairs,
   not raw edges. Each pair is exactly one of:

| Category | Meaning |
|---|---|
| `implicit` | the callee is called by the language, not written at a call site (see below). Checked first; never a miss. A pair is `implicit` only when every call folded into it is. |
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

- **C**: none.
- **C++**: constructors, destructors, `operator...` and compiler-generated
  static initialisers (`_GLOBAL__sub_I_*`,
  `__static_initialization_and_destruction_*`).
- **Rust**: `drop`/`drop_in_place` and methods of std traits (`core::ops`,
  `core::iter`, `core::fmt`, `core::cmp`, `core::hash`).
- **Go**: `init`, including the numbered `init.N` the compiler gives a
  package's several init functions (but not `init.funcN`, a closure).
- **Python**: `__dunder__` methods except `__init__` (a call to a class is
  scored as a call to its `__init__`, own or inherited, as `tests/accuracy`
  does), and property accessors (`is_property` or a property-like decorator).
- **PHP**: `__construct`, `__destruct` and the other magic methods, plus
  `offsetGet`, `offsetSet`, `offsetExists`, `offsetUnset` and `getIterator`
  (the engine calls them for `$x[..]`, `isset`, `unset` and `foreach`; a plain
  function such as the `__()` helper is not implicit).
- **TypeScript**: `constructor` and getters/setters (also an accessor
  descriptor's `get k`/`set k`).

A capture that shows more extends `is_implicit` in `runtime.py` and this list.

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

Not part of `task test` or CI: it needs clones and, for a new capture, the
tracer of that language. The unit tests (`tests/unit/test_runtime_compare.py`,
`test_runtime_capture.py`) run on synthetic data and are in CI.

## Recorded projects

| Project | Language | Tool | Workload | Pairs | Found | Wrong target | Missing | Recall | By calls |
|---|---|---|---|---:|---:|---:|---:|---:|---:|
| redis/redis 8.10.0 | C | valgrind callgrind 3.22.0 | redis-server (`-O0 -fno-inline`, libc malloc) driven by `capture/c_workload.redis` and `redis-benchmark -n 500` (`capture/c.sh`) | 5,581 | 5,098 | 18 | 413 | 0.9220 | 0.9919 |
| google/leveldb 1.23 | C++ | valgrind callgrind 3.22.0 | 27 of 30 gtest binaries, cases over 1 s left out (`capture/cpp.sh`) | 5,655 | 2,327 | 1,011 | 34 | 0.6901 | |
| BurntSushi/ripgrep 14.1.1 | Rust | valgrind callgrind 3.22.0 | 24 `rg` searches over `crates/` plus the unit tests of 5 crates (`capture/rust.py`) | 3,848 | 3,175 | 342 | 80 | 0.8827 | 0.8544 |
| gin-gonic/gin v1.12.0 | Go | entry-hook instrumentation, go 1.26 | `go test ./...` (`capture/go.py`) | 2,616 | 2,127 | 232 | 231 | 0.8212 | 0.9975 |
| apache/superset 6.1.0 | Python | `sys.monitoring`, CPython 3.12 | `tests/unit_tests` (`capture/python.sh`) | ~8,000 | 6,837 | 146 | 512 | 0.9122 | 0.9296 |
| laravel/framework v13.23.0 | PHP | Xdebug 3.2 profiler, PHP 8.3 | phpunit `tests/{Support,Container,Pipeline,Events,Config,Routing,Validation}` (`capture/php.sh`) | 18,491 | 7,763 | 326 | 7,046 | 0.5129 | 0.9114 |
| colinhacks/zod v4.4.3 | TypeScript | jsprobe (babel instrumentation, vitest, node 22) | the zod package's vitest suite (`capture/typescript.sh`) | 3,933 | 2,461 | 203 | 728 | 0.7255 | 0.4348 |

What the misses are, by project:

- **redis**: almost all calls through function pointers (`c->cmd->proc`,
  `RedisModule_*`, dictType and rio callbacks, ae events). The wrong_target
  pairs are callix bugs: a `static` prototype plus a non-static definition
  become two nodes, macro-alias callees such as `rax_malloc_usable`, `.c` files
  `#include`d by another `.c`. Unmapped: `src/server.c:main` (callix keeps one
  `main` per root) and `src/commands.def:commandGroupStr` (a generated `.def`
  include).
- **leveldb**: virtual dispatch through `Env`, `Iterator`, `WritableFile` and
  `DB` (wrong_target), function-pointer callbacks (`CleanupNode`, the cache
  deleter), calls in direct-initialised locals (most vexing parse), and
  `c_test.c` calling `c.cc` across adapters. The 94 unmapped `IsOK` calls are a
  gmock MATCHER with no node.
- **ripgrep**: `dyn Trait` dispatch bound to the trait declaration
  (wrong_target), calls inside `macro_rules!` bodies, and closures passed to
  combinators.
- **gin**: interface dispatch (`Binding.Bind`, `Validator.ValidateStruct`) bound
  to the interface method (wrong_target), and calls through function values:
  the handler chain `c.handlers[c.index](c)` and methods called on interface
  values.
- **superset**: polymorphic calls through a class held in a variable
  (`clz._pre_action()`) bound to the base class (wrong_target), and calls on
  receivers whose type the resolver does not know.
- **laravel**: calls on receivers typed only by a return value (fluent
  `collect(...)->first()` chains, `$this->app->singleton`), dynamic method
  names (`$this->{$driverMethod}()`), and overridden methods such as
  `newInstance()` (wrong_target).
- **zod**: v3 `_parse` virtual dispatch, `core.$ZodX.init` bound to the
  interface signature, callbacks (`JSON.stringify` replacers, `flatten`'s
  mapper, `errorMaps`), `(f as any)(...)` (a REFERENCES, not a CALLS), and calls
  of closures returned by other functions. Unmapped: `export default
  function(){}` locales and `"~validate"(){}` have no node.
