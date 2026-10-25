---
sidebar_position: 5
---

# Export formats

An exporter is a function of the graph's JSON — the same document
`Graph.to_json()` writes — and nothing else. Every one streams to its output as
it goes, so a graph with a million nodes never exists a second time as a string,
and every one returns **warnings**: what the target format could not carry, or a
reason to doubt the result. `callix formats` lists them.

| `-f` | Output | For |
|---|---|---|
| `lsif` | LSIF 0.4.3, JSON Lines | code navigation: GitLab, Sourcegraph-style tools |
| `cyclonedx` | CycloneDX 1.5 JSON | an SBOM of the declared dependencies |
| `csv-nodes` | CSV, a row per node | spreadsheets, `COPY` into a database |
| `csv-edges` | CSV, a row per relation, endpoints spelled out | the same |
| `dot` | Graphviz | looking at calls, inheritance and type use |
| `mermaid` | Mermaid flowchart | the same, in a Markdown page |

`dot` and `mermaid` draw the *behavioural* graph only — `CALLS`, `INHERITS_FROM`,
`HAS_TYPE`, `REFERENCES` — because `CONTAINS` and `DECLARES` turn any real
project into a hairball. `--symbol` draws only the edges touching one node.
Mermaid stops rendering past about 500 edges, and the exporter says so.

## LSIF

LSIF is a graph of documents, ranges and results that answers LSP requests —
go to definition, find references, hover — without a language server running.
GitLab reads it as a code-navigation report.

| In the callix graph | In the LSIF dump |
|---|---|
| a file | `document`, with its `languageId` |
| a definition's `metadata["name_span"]` | `range` — the identifier, not the whole extent |
| a reference edge's `metadata["span"]` | `range` at the use-site |
| a definition, or an external symbol | `resultSet` |
| where it is defined, where it is referenced | `definitionResult`, `referenceResult` with `item` edges per document |
| kind and qualified name | `hoverResult` |
| an external symbol | `hoverResult` named after the text at the site, `(external, stdlib)` |
| a named external symbol | `moniker`, kind `import` |

Positions are converted properly: callix counts bytes from 1, LSIF wants UTF-16
units from 0. Give the exporter the sources (the directory you analysed, or
`--source-root` for a saved graph) and every column is exact; without them
columns are emitted as byte offsets, which is wrong on lines holding non-ASCII
text, and the exporter warns.

### What an LSIF dump cannot hold

These are limits of the graph, not bugs in the exporter, and each one produces a
warning:

- **No references without resolution.** A reference is a resolver's answer. A
  graph built with `--no-resolve`, or whose toolchain was missing, exports its
  definitions only.
- **C and C++ references carry no position.** The symbol table that resolves
  them records the edge but not where it was written, so a C or C++ dump has
  definitions and hovers but no find-references.
- **Hover is thin.** The graph keeps no docstrings or signatures, so a hover is
  the symbol's kind and qualified name.
- **One project per language.** A polyglot tree is written as several LSIF
  `project` vertices, which is valid, but not every consumer accepts it. Index
  per language with `-l` if yours does not.
- **Degraded languages.** PHP, C and C++ are resolved by a symbol table and the
  dump says so in its warnings; treat their references as approximate.

The structural rules a consumer relies on — ascending ids, edges that only point
backwards, `item` edges inside their document — are checked by the test suite
with `callix_export::lsif::check`.

## CycloneDX

The graph knows the dependencies a project **declares**: a name each, as its
manifests list them. It records neither a version nor an ecosystem, so:

- components carry no `version`, and the exporter warns;
- the package-URL type comes from the source files when they are all one
  language (`pkg:pypi/…`, `pkg:npm/…`, `pkg:golang/…`, `pkg:cargo/…`,
  `pkg:composer/…`), and falls back to `pkg:generic/…` with a warning when the
  tree mixes languages.

That makes it a faithful inventory of what is declared, not a vulnerability
scan's input on its own. For one, point your scanner at the lockfile.
