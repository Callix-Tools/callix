---
sidebar_position: 1
---

# Nodes

A node is a flat record with a `kind` discriminator rather than a class
hierarchy — cheaper to build in a hot loop, and trivial to serialize.

```python
node.id              # str, deterministic
node.kind            # NodeKind
node.qualified_name  # str, unique within (project, kind)
node.name            # str, the short name
node.file_path       # str | None, always relative to the project root
node.span            # Span | None
node.metadata        # dict[str, object]
```

## The 14 kinds

| Kind | Meaning |
|---|---|
| `PROJECT` | one analysed root |
| `MODULE` | a namespace — the shape differs per language, see below |
| `FILE` | a source file |
| `CLASS` | class, struct, enum, trait, interface |
| `FUNCTION` | a free function |
| `METHOD` | a function bound to a type |
| `VARIABLE` | module- or package-level binding |
| `ATTRIBUTE` | a field on a type |
| `PARAMETER` | a function parameter |
| `TYPE_ALIAS` | a named alias for a type |
| `IMPORT` | an import statement |
| `EXTERNAL_SYMBOL` | a target outside the graph |
| `DEPENDENCY` | a package the manifests declare, whether or not anything imports it |
| `BOUNDARY` | a cross-service port |

`MODULE` is the kind whose meaning moves most between languages:

| Language | A `MODULE` is |
|---|---|
| Python, TypeScript | a dotted name — `app.services.billing` |
| Go | a package directory, whose `qualified_name` **is** the import path |
| Rust | a module path — `crate::net::http` |
| PHP | a namespace — `App\Service` |
| C, C++ | the directory; a C++ namespace lives in the qualified names instead |
| YAML | none — a `FILE` hangs off the `PROJECT` directly |

`CLASS` is similarly wide: a PHP interface, trait and enum are all `CLASS` with a
metadata flag, and so are a C struct, union and enum. The kind is a slot in the
graph, not a claim that the languages agree.

## Identifiers

```
id = sha256(f"{project}::{kind}::{qualified_name}")[:16]
```

Nothing machine-specific enters it — no absolute path, no timestamp, no run
counter. Two runs on unchanged source produce identical IDs, which is what
makes diffing, merging and incremental updates possible.

`BOUNDARY` is the exception, and deliberately so:

```
id = sha256(f"boundary::{mechanism}::{key}")[:16]
```

No project and no language, so the same contract seen from two services
collapses into one node.

## Spans

Every span is **1-based** on both line and column, matching what an editor
shows. Tree-sitter reports 0-based positions; the conversion happens once, at
the visitor boundary.

`node.span` covers the whole declaration. `node.metadata["name_span"]` covers
just the identifier, and that is the one resolution uses: a resolver answers
with the position of a definition's *name*, so the lookup has to be against
name spans.

## Paths are relative, always

`file_path` is relative to the root passed to `analyze()`, on every node that
has one. Nothing machine-specific may enter a graph: an absolute path would make
two runs of the same source on two machines produce different values, which
breaks diffing and merging — and would quietly break `nodes_in_file()` for
callers who pass the relative path the FILE node advertises.

## Ordering

Nodes come out in insertion order, not sorted and not hashed. That is part of
the value: it keeps serialization byte-stable and keeps `CONTAINS` edges in a
readable sequence.
