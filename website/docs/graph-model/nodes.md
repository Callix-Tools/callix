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
| Go | a package directory, whose `qualified_name` **is** the import path; it carries the `file_path` of its first non-test file and a `name_span` at the package clause, and no `span` |
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

`node.metadata["declared_at"]`, when present, lists further name spans that resolve to
this same node: the signatures of an overloaded function, and in Rust the later
declarations of a qualified name in the same file, such as the `add` of `impl Add<S>`
and of `impl Add<&S>` for one type, or the variants of a `cfg`-gated function. A
resolver's answer at any of them lands on the node.

## Documentation

`node.metadata["doc"]` holds what the author wrote about a definition, as plain
text with the comment markers removed. It is on classes, functions, methods,
variables, attributes and type aliases, and absent when there is nothing:

| Where it is read from | Languages |
|---|---|
| the **docstring**, first statement of a function or class body (cleaned the way PEP 257 describes) | Python |
| the **comment block directly above** the definition: `///`, `/** … */`, `//`, `#` | all of them |

"Directly above" means the last line of the comment is the line before the
definition, with only attributes or decorators between them; a blank line ends
the block. A comment that trails another statement (`x = 1  # why`) belongs to
that statement, not to the next one. In C and C++ a definition with no comment
takes the one on its prototype, since that is where headers keep it; the
definition's own comment wins when both exist.

```python
node.metadata["doc"]    # 'Create a member.\n\nRaises ValueError if the name is empty.'
```

A Python **module's docstring** is the `doc` of its MODULE and of its FILE.

Everything else the author wrote is kept in separate keys, and none of them
replaces `doc`:

| Key | What it holds | Where |
|---|---|---|
| `leading_comment` | the comment block directly above, when a docstring took `doc` | Python definitions |
| `comments` | the comments written inside the body, as `[{"line": 12, "text": "…"}]`: one entry per block of adjacent comment lines, or per comment that follows code; the comments of a definition nested in it are its own; at most 200 | classes, functions, methods, variables, attributes (Python, TypeScript, PHP, Go, Rust) |
| `trailing_comment` | the comment after a definition on its line: `x = 1  # why` | variables and attributes |

```python
node.metadata["comments"]          # [{'line': 7, 'text': 'a second explanation, inside the body'}]
node.metadata["trailing_comment"]  # 'the commit happens in the caller'
```

License headers, `TODO` notes outside any definition and the comments of C and
C++ bodies are not recorded.

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
