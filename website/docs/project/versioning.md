---
sidebar_position: 5
---

# What a version number promises

callix follows semantic versioning, which is only useful if it is clear what
the version is *about*. A code-analysis tool has two very different surfaces:
the vocabulary it speaks, and the answers it gives. Only the first can be
stabilised, because the second depends on type checkers that are themselves
still moving.

## Covered by semver

### The graph contract

Everything a consumer has to hard-code in order to read a graph.

| | Where it lives |
|---|---|
| The 14 `NodeKind` values | [`src/node.rs`](https://github.com/Callix-Tools/callix/blob/main/src/node.rs) |
| The 12 `RelationKind` values | [`src/relation.rs`](https://github.com/Callix-Tools/callix/blob/main/src/relation.rs) |
| `sha256("{project}::{kind}::{qualified_name}")[:16]` | [`src/ids.rs`](https://github.com/Callix-Tools/callix/blob/main/src/ids.rs) |
| `sha256("boundary::{mechanism}::{key}")[:16]` | same |
| Spans are 1-based, in lines and columns | [`src/span.rs`](https://github.com/Callix-Tools/callix/blob/main/src/span.rs) |
| Node insertion order is part of the output | `Graph` stores nodes in an `IndexMap` |
| `file_path` is always relative to the project root | every adapter |

The two id formulas are the load-bearing ones. Change either and every stored
graph is silently invalidated — a node that used to be `a153ff68a458153f` is
now something else, and a diff against yesterday's graph reports the whole
project as rewritten. Both are pinned by unit tests against digests computed
independently in CPython, so the formula cannot drift by accident.

Node ordering is covered for the same reason: a graph has to serialize
identically on two machines, or it cannot be a cache key, a diff input or a
golden file.

### The Python API

The surface generated into
[`python/callix/_core/__init__.pyi`](https://github.com/Callix-Tools/callix/blob/main/python/callix/_core/__init__.pyi),
which is committed and checked in CI — `task stubs:check` fails on any drift
between the stubs and the Rust source, so the file cannot fall out of date
quietly.

### The serialization format

`SCHEMA_VERSION`, and the shape of what `to_dict` produces.

**`SCHEMA_VERSION` will not change within 1.x.** This is a stronger promise
than it looks, and it is deliberate: `ensure_schema_version` requires *exact*
equality, so a reader rejects any version but its own. There is no
forward compatibility and no backward compatibility across a bump — a graph
written with `schema_version: 1` is unreadable by a callix expecting 2, and
the reverse. Raising it therefore means a major release.

## Not covered by semver

### What the resolvers answer

The edges that resolution produces — `CALLS`, `REFERENCES`, `HAS_TYPE`,
`INHERITS_FROM` — may change in a patch release, and will.

- The Python resolver is [ty](https://github.com/astral-sh/ty), linked in and
  pinned to a `0.0.x` release. Its API is explicitly internal and changes
  without notice, and its *answers* change faster than its API: going from ty
  0.0.52 to 0.0.65 was 641 upstream commits.
- The Rust resolver shells out to `rust-analyzer`, whose version is whatever
  the user has installed.
- The TypeScript resolver is built on typescript-go, which is pre-release.

What *is* watched is the aggregate. Every project baseline records how many
queries the resolver answered — currently between 84.8% on apache/superset and
99.8% on astral-sh/ruff — so a regression shows up as a drop in that number
even though no individual edge is frozen.

### How many boundaries are found

Boundary detection is pattern-based. A release may recognise a framework it
previously missed and so find more boundaries in unchanged source. That is an
improvement, and it is not a breaking change even though the graph grew.

### The node scheme inside a language

A visitor may start emitting a construct it previously ignored — a PHP
attribute, a C++ concept, a decorator form. New nodes and edges appear in a
graph of unchanged source. The *vocabulary* is fixed; how exhaustively each
language is walked is not.

### `ResolverMetrics`

Counts and timings are diagnostics, not a contract.

## The decisions behind the boundary

**Adding a language is a minor release.** An adapter is chosen explicitly —
`PythonAdapter`, `PhpAdapter` — so a new one adds nothing to anybody's existing
graph. Nothing breaks by its existence.

**Removing or renaming a `NodeKind` or `RelationKind` requires a major
release.** Not because of the running code, but because of stored graphs:
deserializing a kind that no longer exists raises `SerializationError`, so
dropping one retroactively breaks every graph on disk that used it.

**Adding a `NodeKind` or `RelationKind` is a minor release** — with a caveat
worth stating plainly. A consumer that matches exhaustively on kinds will see
one it does not know. The graph stays readable; code that assumed the set was
closed does not. This is why `DEPENDENCY`, `DEPENDS_ON` and
`COMMUNICATES_WITH` were implemented before 1.0 rather than after: they were
already declared in the enums and producing nothing, and removing unused
vocabulary later would have been the breaking direction.

**Widening a function signature with a keyword-only argument is a minor
release.** Every adapter constructor is keyword-only *entirely* — there is no
positional argument to shift — precisely so an argument can be added later
without breaking a call. That is also why they were all made to take the same
four (`resolve`, `resolver`, `dep_parsers`, `boundary_extractors`) before 1.0
rather than after: making `PythonAdapter`'s two positional arguments keyword-only
was a breaking change, and it was free to make while the promise was not yet in
force.

## What 1.0 settled first

The project is at **1.0.0** and everything above is now in force. The window in
which breaking changes were cheap is closed, which is why it was spent on the
deliberate divergences recorded in
[the migration notes](./migrating-from-graphlens.md) — a vendored dependency
reported as first-party code, or a duplicated structural edge, was worth fixing
while fixing it was still free. The adapter constructors were aligned in the same
spirit, all eight taking the same keyword-only four: the shape of the public API
is the one thing 1.0 freezes hardest.

The practical consequence is `SCHEMA_VERSION`. It stays at 1 for all of 1.x,
because `ensure_schema_version` demands exact equality — a bump would make every
stored graph unreadable in both directions, so it cannot happen in a minor
release.
