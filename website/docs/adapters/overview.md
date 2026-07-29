---
sidebar_position: 1
---

# Overview

Four adapters, one interface:

| Language | Class | Project marker | Extensions |
|---|---|---|---|
| Python | `PythonAdapter` | `pyproject.toml`, `setup.cfg`, `setup.py` | `.py`, `.pyi` |
| TypeScript | `TypeScriptAdapter` | `package.json`, `tsconfig.json` | `.ts`, `.tsx`, `.mts`, `.cts` |
| Go | `GoAdapter` | `go.mod` | `.go` |
| Rust | `RustAdapter` | `Cargo.toml` | `.rs` |
| YAML | `YamlAdapter` | any `.yaml` / `.yml` | `.yaml`, `.yml` |

[YAML](./yaml.md) is the odd one out: it declares no symbols, so it produces
boundaries and service wiring rather than functions and calls. It earns its
place because an OpenAPI document states routes that no source file mentions.

The other four discover every project root under the path you give them, so a
monorepo with several packages works without configuration. A marker in the
root you pass does **not** hide nested roots — otherwise a monorepo that is
itself a package would swallow its own subprojects.

## The three phases

`analyze()` always runs the same three phases, and the order is not cosmetic:

1. **Structure** for every root, with no resolution. Nodes, containment and
   import edges are created here. This has to finish for the whole workspace
   first, or a cross-root definition would have nothing to bind to.
2. **Resolution**, once for the whole call. One resolver serves every root:
   per-root resolvers would both lose cross-root references and re-index the
   workspace once per root.
3. **Boundaries**, so `BOUNDARY` nodes land in the graph after the resolver's
   edges.

## Node schemes differ

Do not assume Python's shape carries over. A `MODULE` is:

- a dotted name in **Python** and **TypeScript** (`app.services.billing`);
- a **package directory** in **Go**, whose qualified name equals the import
  path — which is why internal imports bind by direct lookup;
- a module path in **Rust** (`crate::net::http`), derived from the file's place
  under `src/`, with inline `mod foo { … }` adding segments.

## Resolver status

Every graph reports how complete it is:

```python
graph.metadata["resolver_status"]   # 'ok' | 'degraded' | 'unavailable'
```

`unavailable` means resolution did not run — either `resolve=False`, or the
toolchain the language needs was missing. `degraded` means it ran and produced
a partial answer. See [Resolvers](./resolvers.md).
