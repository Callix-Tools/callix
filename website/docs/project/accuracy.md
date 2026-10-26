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
| Python | ty, linked in | by types: the definition the checker names at the callee | A function carried in a container, an argument or a return value is followed by callix's own pass (below), not by ty. Dynamic attribute access, `getattr`, `**kwargs` forwarding and monkey-patching are not followed at all. Polymorphic receivers resolve to the declared class, not to each subclass. |
| TypeScript | typescript-go, linked in | by types | Destructured function values, calls through `any`, dynamic `import()` and string-keyed lookups. A callback handed to a library function is not assumed to be called. |
| Go | `go/packages` + `go/types` | by types; needs the Go toolchain | A call through an interface lands on the interface's method, never on an implementation. A function reached through a map index or a channel is followed only by the same pass. |
| Rust | `rust-analyzer scip` | by types; needs rust-analyzer and Cargo | A call through a trait or a trait object lands on the trait's method, not on an impl. Macro-generated items. |
| PHP | a symbol table over the sources | by the declared type of the receiver, a `new` assigned to it, and namespaces | A method called on a variable with no declared type has no edge. A callable named by a string is not followed. |
| C, C++ | a symbol table over the sources | by the receiver's declared type and the class's bases, a written scope, an unqualified name in a method meaning the class first, `#include` visibility, and the call's arity and the look of its arguments for overloads | Function pointers, `std::function`, template-dependent names, macros, the preprocessor. An overloaded operator, a functor and `delete` are bound by the declared type of the variable, so only when it holds an object or a pointer the function can see declared. A receiver whose type nothing declares falls back to the bare name, and the edge says `ambiguous` when the name is not unique. |

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
| `name` | Matched by name in a symbol table, with no type information. `ambiguous: true` when several declarations carried the name and the first was taken. |

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

It is a small, flow-insensitive and context-insensitive pass that runs after
resolution. A variable assigned two functions holds both; a parameter holds the
argument of every call site that passes it. It follows names, attribute chains,
container literals and `table[key]`, the names a function returns and the result
of a call that is called. It over-approximates where a call graph usually does,
and the label says so. It follows nothing it cannot resolve to a node of the
project.

## Measured accuracy

`tests/accuracy` holds a suite of small programs per language, each with the call
graph a reader of the source would draw, and scores the graph callix builds
against it. Python uses the [PyCG micro-benchmark](https://github.com/vitsalis/pycg-evaluation)
(112 cases, vendored with its licence); the other languages have suites written
for this repository from each language's rules, not from what callix answers.

A case is **sound** when every expected edge is present and **complete** when no
edge from a listed caller is unexpected. Edge **recall** and **precision** are
computed over calls to a definition of the program. The numbers at the time of
writing:

| Suite | Cases | Sound | Complete | Edge recall | Edge precision |
|---|---:|---:|---:|---:|---:|
| Python (PyCG) | 112 | 80 | 107 | 0.79 | 0.97 |
| TypeScript | 49 | 47 | 49 | 0.97 | 1.00 |
| Go | 48 | 47 | 48 | 0.99 | 1.00 |
| Rust | 43 | 42 | 43 | 0.98 | 1.00 |
| PHP | 42 | 38 | 42 | 0.94 | 1.00 |
| C++ | 40 | 38 | 40 | 0.97 | 1.00 |
| C | 27 | 25 | 27 | 0.95 | 1.00 |

How to read them:

- These are **micro-benchmarks**. Each case isolates one language feature. A high
  score says the feature is handled, not that a real project is resolved at that
  rate. On the five real Python projects of the PyCG macro-benchmark, the edges
  callix can name (calls to a function of the project) are found at a recall of
  about 0.81, and about 0.94 to 0.98 of the edges it draws are expected, depending
  on whether a call that stopped at a variable counts as wrong. PyCG's own paper
  reports 70 % recall and 99 % precision over *all* edges, library calls included,
  so the two are not the same measure and are not a comparison.
- The suites for Go, Rust, TypeScript, PHP, C and C++ were written *after* looking
  at what the resolvers did badly, so they cover the features that had been
  getting through. They are not a sample of how code is written.
- Precision is near 1.0 because a resolver either names a callee it found or says
  nothing. The honest cost of that is recall: for Python, the 49 calls that stop
  at a variable, and the function values no pass follows, are the 21 % missing.
- A callee outside the program has no name in the graph, so it is scored apart
  and leniently, and is not in these numbers.

What the C and C++ suites do not show is how often a call lands on the wrong
class in real code. That is what `ambiguous` is for: leveldb has 417 such edges
in 4 375 matched by name.

### What the suites still miss

The `idioms/` directory of each language holds the ways code is commonly written
that a first pass did not cover. The cases that still fail are kept, with their
expected edge, so that the number does not flatter:

- **C++**: a function pointer or a `std::function` called through its variable
  (tree-sitter-cpp reads `void (*f)() = g;` as an expression), macros, and a
  `delete` through a base pointer, which lands on the base's destructor because
  only the declared type is known.
- **PHP**: a callable named by a string or an array (`'f'`, `[$o, 'm']`), whether it
  is handed to a built-in such as `array_map` or to a function that calls its
  parameter.
- **Python**: a function fetched from a container by an index the pass cannot
  tell apart from another, a polymorphic receiver (it lands on the declared
  class, not each subclass), `super(Cls, self)`.
- **C and C++**: a callback handed to a library function (`qsort`, `.map(f)`) is a
  reference to the function, not a call: whether the library calls it is
  unknowable from the program.

### The floor

`tests/accuracy/thresholds.json` records the score each suite had when last
raised, and `pytest` (so `task unit` and CI) fails when a suite falls below it.
`task accuracy` prints the table, `task accuracy -- go/micro -v` every miss, and
`task accuracy:update` raises the floor after an improvement. A change that
trades one number for another has to say so: `run.py --reset` writes the
measured values over the floor, and the diff is where a reviewer sees what was
given up.

## What this does not tell you

- Nothing here covers code generated at run time, reflection, or a framework's
  dependency injection by name.
- The ground truth is written by people who read the code; where two readers
  could disagree (what a call through an interface means) the convention is in
  `tests/accuracy/README.md`.
- A better resolver can be plugged in with `resolver=`. The accuracy suite is
  how to find out whether it is better.
