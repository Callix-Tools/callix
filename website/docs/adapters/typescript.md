---
sidebar_position: 4
---

# TypeScript adapter

```python
from callix import TypeScriptAdapter

graph = TypeScriptAdapter().analyze("path/to/ts-project")
TypeScriptAdapter(resolve=False).analyze(root)      # structure only
```

| Property | Value |
|---|---|
| Language id | `typescript` |
| Project markers | `package.json`, `tsconfig.json` |
| Extensions | `.ts`, `.tsx`, `.mts`, `.cts` |
| Resolver | embedded typescript-go |
| External requirements | none |

Declaration files (`.d.ts`, `.d.mts`, `.d.cts`) are skipped: they carry types
without implementation.

## What it extracts

Classes, interfaces and their members, enums, type aliases, functions and
arrow functions, variables from `const`/`let` declarations, and imports and
re-exports — including `export { X } from 'mod'`, whose specifier sits directly
on the export statement rather than in a `from` clause.

## Module names and path aliases

A file path becomes a dotted module name: `src/pkg/ui.tsx` → `pkg.ui`. Files
named `index` represent the package itself.

`compilerOptions.paths` is honoured. `tsconfig.json` is parsed as JSONC —
`//` comments and trailing commas included, because that is how the file is
written in practice. An alias is rewritten before the module name is computed,
so `@/client/v2` → `src/client/v2` → `client.v2`, which lines up with the names
derived from file paths.

## Dependency classification

`dependencies`, `devDependencies`, `peerDependencies` and
`optionalDependencies` are all read, so imports from tests and peer
dependencies classify as `third_party` rather than `unknown`. Node's built-in
modules are recognized with and without the `node:` scheme.

## Boundaries

Express routes and NestJS `@Get(...)` controller methods on the server side;
`fetch` and axios on the client side; queue producers and consumers.

A `fetch(url, {method: "POST"})` is read as POST — the literal wins over the
GET default, so a non-GET request does not enter the graph as a GET.

## Members and object-literal keys

A property read (`s.kind`) and a property write (`s.size = 3`) give a `REFERENCES`
edge to the property they name, with `access` `read` or `write`, when the type
checker can place it (an interface member, a class field, a method). A property
of a library type becomes an external symbol, as a library call does.

The key of an object literal, and the name of a method written in one, references
the member of the type the literal is checked against (`const h: Shape = { kind: 1 }`,
a returned literal, `{...} as Shape`); its `access` is `read`. The key of an
untyped literal declares the property and gives no edge, and a key typed by a
library interface gives none either.

Not covered: element access (`o["k"]`, `o[k]`), shorthand keys (`{ x }` references the
local), computed keys, `x.count++` as a write, and members of a type literal, which
have no node yet, so reads of them are `unlocated`.

A method or accessor of an object literal that is not a variable's initializer (a call argument, a returned or nested literal) is an inline closure: its calls belong to the enclosing function, as a callback's do. A comment in front of an initializer (`/*@__PURE__*/`) does not affect what is scanned.

## Type positions

Every name a type mentions is a use, not only the first identifier. In a declaration
position it is a `HAS_TYPE` edge: a variable, field or parameter annotation, a return
type, the body of a type alias, and the constraint or default of a type parameter
(`a.T`, `Array<Shape>`, `Shape | a.T`, `Shape["k"]`, `{ s: Shape }` each give an edge per
name). The type of `a.T` is `T`; the namespace `a` and the operand of `typeof v` are
`REFERENCES` with `access` `read`.

Where the type is not the type of a declaration it is a `REFERENCES` edge: the type
arguments of a call or `new` (`f<Shape>(x)`), `x as T`, `x satisfies T`, `<T>x`, `f<T>`,
and the type arguments and namespace prefix of an `extends` clause. An interface that
extends a qualified name (`interface J extends a.Shape`) now inherits from it.

A type parameter, a mapped-type key (`[k in keyof S]`) and an `infer` name are not nodes
and give no edge; neither do the parameter names inside a function type.

Not covered: `implements` clauses, the annotations of inline arrow-function parameters,
and JSDoc types in `.js` files.

## The embedded typescript-go

The resolver is built on
[typescript-go](https://github.com/microsoft/typescript-go), the official Go
port of the TypeScript compiler, linked into the module statically together
with `lib.d.ts`. Neither `tsc` nor `tsgo` nor Node is needed.

It can also be used directly:

```python
from callix import TsResolver

resolver = TsResolver()
resolver.prepare("path/to/ts-project")     # finds tsconfig.json itself
resolver.definition_at("src/main.ts", 4, 15)
```

The first query builds the program and takes tens of milliseconds; each one
after that is tens of microseconds.
