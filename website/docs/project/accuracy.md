---
sidebar_position: 3
---

# How accurate resolution is

A resolver is not right or wrong as a whole. It finds some edges, misses others,
and now and then finds one that is not there. callix measures that, per language,
against call graphs written by hand, and the graph itself says how each edge was
found. This page is what those two things amount to.

## What each language's resolver actually does

| Language | Backend | How a call is found | What it does not follow |
|---|---|---|---|
| Python | ty, linked in | by types: the definition the checker names at the callee | A function carried in a container, an argument or a return value is followed by callix's own pass (below), not by ty. Dynamic attribute access, `getattr` and monkey-patching are not followed at all. A receiver whose class ty knows resolves to the declared class, not to each subclass. Keyword argument names bind to fields only, not to parameters. |
| TypeScript | typescript-go, linked in | by types | Calls through `any`, dynamic `import()` and computed keys. Destructuring, literal keys and indexes, `bind`/`call`/`apply`, a property a constructor assigns, an object handed to a destructured parameter and a closed list of library callbacks (`map`, `forEach`, `then`, `setTimeout`, `addEventListener`, …) are followed by the flow pass; nested patterns are not read. |
| Go | `go/packages` + `go/types` | by types; needs the Go toolchain | A call through an interface lands on the interface's method, never on an implementation. A function reached through a slice, a range loop, a channel or a variadic parameter is followed by the flow pass, and a closed list of standard-library callbacks (`sort.Slice`, `strings.Map`, `sync.Once.Do`, …) is a call. |
| Rust | `rust-analyzer scip` | by types; needs rust-analyzer and Cargo | A call through a trait or a trait object lands on the trait's method, not on an impl. Implicit calls (`Drop::drop`, an operator's trait method, `Iterator::next` in a `for`) are not edges. The calls in a macro's arguments are found, nested std macros included; the invocation of a project `macro_rules!` is an edge to it (an item-level one from its file; invocations inside `impl` and `trait` bodies are not recorded); a `macro_rules!` of the same file is expanded; a macro from another file, a procedural macro and macro-generated items are not expanded. Same-named methods of several impls of one type (different traits or trait arguments, cfg variants) are one node, and a call to any of them is an edge to it. |
| PHP | a symbol table over the sources | by the declared type of the receiver, a `new` assigned to it, and namespaces; an undeclared method of a class that declares `__call` (or `__callStatic`) is a call of that | A method called on a variable with no declared type has no edge. A callable written out (a string, `'A::m'`, `[$o, 'm']`, `g(...)`) is followed to the parameter it is handed to and to the locals and properties that hold it, and is a call where that parameter, local or property is called, by the flow pass; `$m` and `$class` are followed only while nothing else is assigned to them. |
| C, C++ | a symbol table over the sources | by the receiver's declared type (calls, and field reads, writes and initialisers, through field chains, typedefs and file-scope variables) and the class's bases, a written scope, an unqualified name in a method meaning the class first, `#include` visibility, and the call's arity and the look of its arguments for overloads | Template-dependent names, the preprocessor's conditionals. A macro of the project is expanded (its body with the arguments in place, `##` joined), at the invocation; one that expands to nothing, an attribute or a storage keyword is erased where it sits in a declaration. A functor and `delete` are bound by the declared type of the variable, so only when it holds an object or a pointer the function can see declared. An overloaded operator (assignment, compound assignment, binary and subscript operators) is bound as a member through the class and its bases, or as a namespace-scope function by the classes of its operands; operators on an expression result, unary operators, argument-dependent lookup and templates are not followed, and an operator nothing declares gives no edge. A receiver whose type nothing declares falls back to the bare name, and the edge says `ambiguous` when the name is not unique. A field is never the answer for an unqualified name. |

"Resolved" never means "correct". `resolver_status: ok` means the backend came
up; `degraded` means the backend is a table of names, not a type checker.

## What an edge says about itself

Every `CALLS`, `REFERENCES`, `HAS_TYPE` and `INHERITS_FROM` edge a resolution
pass writes has `metadata["resolution"]`:

| Value | Means |
|---|---|
| `definition` | The backend located a definition in the project and the edge points at it. |
| `value` | A call whose target is a variable, a parameter or a field: the backend found what the callee was *read from*, not the callee. |
| `flow` | A `value` call followed, through assignments, arguments and returns, to a callable it can be shown to hold. `via` names the holder. |
| `external` | The definition is outside the project: the standard library, a dependency. |
| `unlocated` | The backend answered, but with a place callix could not bind to a node (a local it does not model, a position inside an import). The edge points at a synthetic external symbol keyed by the call site. |
| `name` | Matched by name in a symbol table, or by path (Rust `use` paths and `mod x;`, in the structure pass), with no type information. `ambiguous: true` when several declarations carried the name and the first was taken. |

`callix report <graph> -t status` counts the edges by these values, and
`callix analyze` prints where a language's calls went:

```
python      ok            8 360 nodes     16 799 relations     1.7s
            2 404 calls: 1 052 to a definition, of which 16 followed through a value,
            194 stopped at a value, 1 151 outside the project, 7 not bound
cpp         degraded     11 508 nodes     21 891 relations     1.2s
            9 370 calls: 0 to a definition, 4 375 matched by name, 417 of them ambiguous,
            43 stopped at a value, 4 952 outside the project
```

Read the second line of each, not the first. A `degraded` language with 417
ambiguous edges out of 4 375 is telling you which ones to distrust.

### What `flow` is

It is a small, flow-insensitive and context-insensitive pass (with one narrow exception: a PHP parameter assigned by a top-level statement of the body, before it is called, holds only that assignment at the call) that runs after
resolution. A variable assigned two functions holds both; a parameter holds the
argument of every call site that passes it. It follows names, attribute chains,
container literals and `table[key]`, the names a function returns, the result
of a call that is called, a call's result handed on as an argument, a call made
through a parameter that holds a function (its own arguments reach that
function's parameters), and the iteration protocol: `for x in it` calls
`__iter__` and `__next__` of the class `it` is shown to hold an instance of.
Beyond that it follows a decorator (a decorated name stands for what the decorator
returns, applied from the bottom up), a method called on a receiver (found on the
classes the receiver holds an instance of, and on what they inherit), keyed
containers (`d["a"]` and `l[1]` give back what was stored under that key,
including through `append`, `update` and item assignment), unpacking, parameter
defaults, and a closed list of library functions that call an argument (`map`,
`filter`, `sorted(key=)`, `functools.reduce`, `itertools.starmap`, …). It over-approximates where a call graph usually does,
and the label says so. It follows nothing it cannot resolve to a node of the
project.

## Measured accuracy

`tests/accuracy` holds a suite of small programs per language, each with the call
graph a reader of the source would draw, and scores the graph callix builds
against it. Python uses the [PyCG micro-benchmark](https://github.com/vitsalis/pycg-evaluation)
(112 cases, vendored with its licence) plus a small suite of its own for cases PyCG
does not cover; the other languages have suites written for this repository from
each language's rules, not from what callix answers.

A case is **sound** when every expected edge is present and **complete** when no
edge from a listed caller is unexpected. Edge **recall** and **precision** are
computed over calls to a definition of the program. The numbers at the time of
writing:

| Suite | Cases | Sound | Complete | Edge recall | Edge precision |
|---|---:|---:|---:|---:|---:|
| Python (PyCG) | 112 | 112 | 112 | 1.00 | 1.00 |
| Python (micro) | 1 | 1 | 1 | 1.00 | 1.00 |
| TypeScript | 75 | 75 | 75 | 1.00 | 1.00 |
| Go | 63 | 63 | 63 | 1.00 | 1.00 |
| Rust | 62 | 62 | 62 | 1.00 | 1.00 |
| PHP | 71 | 71 | 71 | 1.00 | 1.00 |
| C++ | 57 | 57 | 57 | 1.00 | 1.00 |
| C | 43 | 43 | 43 | 1.00 | 1.00 |

How to read them:

- These are **micro-benchmarks**. Each case isolates one language feature. A high
  score says the feature is handled, not that a real project is resolved at that
  rate. On the five real Python projects of the PyCG macro-benchmark, the edges
  callix can name (calls to a function of the project) are found at a recall of
  about 0.81, and about 0.94 to 0.98 of the edges it draws are expected, depending
  on whether a call that stopped at a variable counts as wrong. PyCG's own paper
  reports 70 % recall and 99 % precision over *all* edges, library calls included,
  so the two are not the same measure and are not a comparison.
- **The Python row is scored against a corrected expectation in one case.** In
  PyCG's `decorators/nested_decorators`, `func` is decorated twice and then called
  from the module, and the vendored ground truth lists the module as calling the
  undecorated `main.func` as well. Python rebinds the name, so the call reaches
  `dec1.inner`, which reaches `dec2.inner`, which reaches `func`; the module never
  calls `func` itself. The vendored file is untouched, the correction is in
  `suites/python/pycg/corrections.json` with that reason, and the run prints it.
  Against the unmodified ground truth the row is 111 of 112 sound. A suite this
  small is also one a resolver can be fitted to: that is why the numbers are
  kept next to the rule that no case's expectation is bent to match the tool.
- The suites for Go, Rust, TypeScript, PHP, C and C++ were written *after* looking
  at what the resolvers did badly, so they cover the features that had been
  getting through. They are not a sample of how code is written.
- Precision is near 1.0 because a resolver either names a callee it found or says
  nothing. The honest cost of that is recall, which on Python's micro-benchmark
  is now complete; on real code the calls that stop at a variable are what is
  missing.
- A callee outside the program has no name in the graph, so it is scored apart
  and leniently, and is not in these numbers.

What the C and C++ suites do not show is how often a call lands on the wrong
class in real code. That is what `ambiguous` is for: leveldb has 417 such edges
in 4 375 matched by name.

### What the suites still miss

The `idioms/` directory of each language holds the ways code is commonly written
that a first pass did not cover. The cases that still fail are kept, with their
expected edge, so that the number does not flatter:

- **C++**: a `delete` through a base pointer lands on the base's destructor, because
  only the declared type is known. An implicit copy or move constructor is not an
  edge: whether a by-value argument copies depends on the callee's signature and on
  elision, and the table does not look past the callee's name.
- **PHP**: a flow-insensitive pass, except that a parameter assigned by a top-level
  statement of the body before it is called holds only what was assigned there. A
  reassignment inside a branch or loop, after the call, by reference, or one that reads
  the parameter, and a parameter passed on or copied after the reassignment, still
  hold both the caller's value and the assigned one. `$m` and
  `$class` are followed only while a variable has one assignment, and a callable in a
  property is followed only through `$this->name` of the declaring class. Magic
  methods are taken as the callee only when no class in the hierarchy has a base
  outside the project, since that base may declare the method.
- **Python**: the micro-benchmark has no failing case left, which says the features
  it isolates are handled and nothing about code that does not look like them. Known
  limits: the pass is flow- and context-insensitive, so a name reassigned between
  two calls holds both values; a library function that calls an argument is followed
  only for the closed list above, and only where the callee is not a function of the
  project (a project function named `map` is left alone); `getattr` and `eval` are
  not followed.
- **C and C++**: a callback handed to a library function is a call only for a closed
  list of them (`qsort`, `bsearch`, `signal`, `pthread_create`, `std::sort`,
  `std::transform`, `std::thread`, …); handed to any other function it is a reference,
  because whether that function calls it is unknowable from the program. A macro of the
  project is expanded where it is invoked; a macro from a system header is not, and a
  macro whose body is not valid C on its own (an unbalanced brace) is skipped. Every arm
  of an `#if` is expanded, so two definitions of one macro are both followed.

### The floor

`tests/accuracy/thresholds.json` records the score each suite had when last
raised, and `pytest` (so `task unit` and CI) fails when a suite falls below it.
`task accuracy` prints the table, `task accuracy -- go/micro -v` every miss, and
`task accuracy:update` raises the floor after an improvement. A change that
trades one number for another has to say so: `run.py --reset` writes the
measured values over the floor, and the diff is where a reviewer sees what was
given up.

## Runtime call graphs

The suites above are written by people reading code; the SCIP comparison asks
static indexers. A third reference is what a program really called: call graphs
recorded while a real project ran its own tests or a fixed workload, stored as
small artifacts in `tests/runtime/`. `runtime.py` maps every recorded function
to a node and scores each executed call as `found`, `wrong_target` (callix has
a call to another node of the same name, such as an interface method),
`missing`, `unmapped` (an end has no node) or `implicit` (constructors,
destructors, `Drop`, operators and magic methods, which the language calls and
no one writes).

Only recall is scored, `found / (found + wrong_target + missing)`: a trace is a
lower bound, so an edge callix has that the run never took is not an error.
It needs clones and is not part of CI.

```bash
task runtime           # compare + check against the recorded floors
task runtime:update    # raise the floors after an improvement
```

The format, the capture rules and the mapping are in
[`tests/runtime/README.md`](https://github.com/Callix-Tools/callix/blob/main/tests/runtime/README.md).

## What this does not tell you

- Nothing here covers code generated at run time, reflection, or a framework's
  dependency injection by name.
- The ground truth is written by people who read the code; where two readers
  could disagree (what a call through an interface means) the convention is in
  `tests/accuracy/README.md`.
- A better resolver can be plugged in with `resolver=`. The accuracy suite is
  how to find out whether it is better.
