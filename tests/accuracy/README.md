# Accuracy

The golden files and the baselines freeze what callix produces. This directory
asks the other question: **is it right?** Every case is a small program with
`callgraph.json` beside it, the call graph a reader of the source would draw, and
a case is scored by comparing the graph callix builds with that file.

```bash
task accuracy                                  # every suite, with the resolver on
task accuracy -- go/micro -v                   # one suite, every miss spelled out
task accuracy:update                           # raise the floor after an improvement
```

`thresholds.json` is a ratchet. `pytest` (so `task unit` and CI) fails when a suite
scores below the recorded floor; `--update` only ever raises it. A fix that makes
a suite better should land together with the new floor.

## What a score means

Per case, as in the PyCG artifact:

* **sound** — every expected edge is in the output;
* **complete** — every output edge whose caller the expectation lists is expected.

Per suite, edge **recall** and **precision** over the same edges, which move when
a case does not flip.

Only `CALLS` edges to a definition *of the program* are scored. Two things are
reported on their own line instead, so neither inflates a number:

* a call that **landed on a variable or a parameter** (`a = f; a()`): the
  resolver answered "it is `a`", which is not a callee;
* callees **outside the program** (standard library, dependencies): the graph
  gives them no name, so they are matched by the last segment of the identifier
  at the call site. That is lenient and is never part of the headline.

## Suites

| Suite | Cases | Ground truth |
|---|---|---|
| `python/pycg` | 112 | the PyCG micro-benchmark, vendored unmodified (Apache 2.0, see its `NOTICE.md`) |
| `typescript/micro`, `go/micro`, `rust/micro`, `php/micro`, `c/micro`, `cpp/micro` | 21–60 each | written for this repository, from the language's rules rather than from callix's output |

A suite is `<suite>/<category>/<case>/`; `README.md` in a case says what it tests.

A vendored suite whose own ground truth is wrong in a case is not edited: it may
carry `corrections.json` (`{"<category>/<case>": {"remove": [[caller, callee]],
"reason": "..."}}`). A correction can only remove an edge the case lists, the run
prints every one with its reason, and the docs give the score both ways. It is for
an expectation that contradicts the language's rules, never for one the resolver
fails.

### Conventions

Names are the ones callix gives nodes, because a name is a name; what is *chosen*
is which edges a program has.

* **Every function the program defines is a key** of `callgraph.json`, with `[]`
  when it calls nothing the program defines. That is how a callee is known to be
  inside the program, and how an extra edge from a listed caller is caught.
* A call through an interface, trait or base class is an edge to **that
  declaration's method** (`I.m`), not to an implementation: the source says no
  more. A call on a value of the concrete type is the implementation's.
* Calls in a closure, lambda or function literal belong to the function that
  contains it.
* Code in a package-level initializer (Go `var x = f()`) belongs to the variable it initialises.
* `new A()` / `A()` is an edge to the constructor when the class declares one, and
  no edge when it does not.
* A function reached through a variable, argument, return value or container is
  an edge to the function (higher-order cases follow PyCG here). callix's
  type-based resolvers do not follow values, and those cases show it.

| | module-level code | function | method | constructor |
|---|---|---|---|---|
| Python | `main` | `main.f` | `main.C.m` | `main.C.__init__` |
| TypeScript | `main.ts` | `main.f` | `main.C.m` | `main.C.constructor` |
| Go | `m.x` (the var/const it initialises) | `m.f` | `m.T.M` | — |
| Rust | — | `m::f` | `m::T.m` | — |
| PHP | `main.php` | `App\f` | `App\C\m` | `App\C\__construct` |
| C | — | `f`, `file.c::f` if static | — | — |
| C++ | — | `f(int)`, `n::f(int)` | `A::m()` | `A::A(int)` |

## Adding a case

Write the program, write what a reader of it would draw, run the suite. When callix
disagrees, decide who is wrong **before** touching either side: the ground truth
is the language's rules, not what callix does. A case that callix fails is not a
problem; it is the reason the case exists.
