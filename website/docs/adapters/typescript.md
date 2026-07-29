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
