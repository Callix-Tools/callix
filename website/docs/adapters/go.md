---
sidebar_position: 5
---

# Go adapter

```python
from callix import GoAdapter

graph = GoAdapter().analyze("path/to/go-module")
GoAdapter(resolve=False).analyze(root)      # structure only
```

| Property | Value |
|---|---|
| Language id | `go` |
| Project marker | `go.mod` |
| Extensions | `.go` |
| Resolver | `go/packages` + `go/types` |
| External requirements | a Go toolchain on `PATH` |

## A different node scheme

A `MODULE` here is a **Go package** — a directory — and its qualified name is
the import path. That is deliberate: an internal import can then be bound by
looking the path up directly, with no name mangling in between.

A package spans several files, so its `MODULE` has no `span`: it carries the
`file_path` of its first non-test file and a `name_span` at that file's `package`
clause, which is what lets an index format define it.

```
module github.com/acme/svc     →  MODULE github.com/acme/svc
internal/store/store.go        →  MODULE github.com/acme/svc/internal/store
```

## What it extracts

Functions, methods with their receiver type in the qualified name, structs and
interfaces, type aliases, constants and variables, and imports. Embedded types
in structs and interfaces become `base` occurrences and resolve to
`INHERITS_FROM`. An embedded field of a struct is also an `ATTRIBUTE` named
after its type's last identifier (`Engine.Base` for `Base`, `*Base`,
`pkg.Base` or `Base[T]`), with a `has_type` to that type. A selector
(`e.Base`), a keyed-literal key (`Engine{Base: b}`) and an argument therefore
bind to the field like a named one.

Every name of a multi-name field (`top, right int`) is its own `ATTRIBUTE`. A field whose type is an anonymous struct, also behind `*`, `[]`, `[N]`, a map value or a chan, has its fields as `ATTRIBUTE`s named `T.F.<anon>.G`, declared by the field `T.F`; there is no node for the struct itself, and a type embedded in it has no `INHERITS_FROM`. Not covered: a package-level `var x struct{...}`, struct types inside function bodies, a composite literal's own `struct{...}{...}` type, `type A = struct{...}`, anonymous structs in generic arguments, function signatures or map keys.

Embedding is distinguished from generic constraints by term count: a type-set
union like `A | B` or `~int` has several terms in one element, real embedding
has exactly one.

The initializer of a package-level `var` or `const` is scanned with the variable as
the enclosing definition: its calls, selectors and types are edges from the
variable. `var f = func(t T) {...}` makes the literal the variable's body, with its
parameters as `PARAMETER` nodes. In a spec without one value per name
(`var a, b = f()`) the first name is the source.

Type uses become `annotation` occurrences and resolve to `HAS_TYPE`: in
signatures, receivers, fields, type declarations (right-hand side and
constraints), `var` and `const` types and initializers, and function bodies (composite literals,
closures, assertions, type switches, `make`/`new`, type arguments). Predeclared
types, type parameters, types declared inside a function and the generic of an
explicitly instantiated call `F[T](x)` get no edge.

Package names are references too. In `util.Double(2)` or `t util.T`, the
qualifier `util` becomes a `REFERENCES` edge to the package's `MODULE` (or, for a
standard-library or third-party import, to the import's external symbol), with the
span of the qualifier. The import path literal is the `span` of the import's
`RESOLVES_TO`. A qualifier is accepted only when the type checker answers with the
position of an import, so a local variable that shadows a package gets no edge.
In expressions the candidate names are the import aliases and the name Go assumes
from the path (`gopkg.in/yaml.v3` is `yaml`); an unaliased package that declares a
different name is missed there. Dot and blank imports bind no qualifier.

Fields are references too. `x.f` is a `REFERENCES` edge with `access` `read` or
`write` to the field, and the key of a keyed struct literal (`T{Name: v}`,
`&T{...}`, the elided `[]T{{Name: v}}`) is a `write` by the enclosing function or,
in a package-level `var`, by the variable. Keys of map, slice and array literals,
and fields of types declared inside a function, get no edge.

So is a bare use of a package-level name. A `const`, `var` or function of the
file's own package used as a plain identifier (an assignment side, an operand, a
return value, a `case` value, a composite-literal element, a `range` operand, the
operand of `v.M`, or `h := helper` for a function value) is a `REFERENCES` edge with
`access` `read` or `write` (`x++` and `x += 1` are writes). Locals, parameters,
predeclared names (`nil`, `true`, `len`) and names brought in by a dot import get
no edge and no external symbol.

## Dependency classification

Go's own rule applies — a standard-library import path has no dot in its first
segment, third-party paths start with a domain. Module paths come from `go.mod`,
from both block and single-line `require` directives.

## Boundaries

gin, chi and echo routes; `net/http` clients; gRPC stubs built through
`New<Service>Client`; Temporal activities; queue publish and subscribe.

## The resolver

`go/packages` and `go/types` are linked into the same Go c-archive as the
TypeScript bridge, so resolution is an in-process call rather than an LSP
conversation. Verified against `gopls`: the answers match down to the column.

Two things it does that are easy to miss:

- **Test files are type-checked.** `packages.Load` runs with `Tests: true`;
  without it every use-site in `*_test.go` stays unresolved. On gin that alone
  is the difference between 16.5% and 95% of use-sites answered.
- **Builtins have a position.** `len`, `make`, `append`, `panic`, the
  conversions, and `error.Error` live in universe scope and carry no source
  position. gopls points at `$GOROOT/src/builtin/builtin.go`, so the bridge
  loads the `builtin` package separately and records its declarations —
  including the methods of builtin interfaces. That takes gin from 95% to
  99.3%.

## Why Go is required

Unlike Python and TypeScript, Go's standard library is **sources in GOROOT**,
and `packages.Load` shells out to `go list`. It cannot be frozen into a wheel.
For a Go project this costs nothing: the toolchain is already there.
