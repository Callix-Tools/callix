---
sidebar_position: 2
---

# Models

## Node

```python
Node(id, kind, qualified_name, name,
     file_path=None, span=None, metadata=None)
```

See [Nodes](../graph-model/nodes.md) for what each field means.

## NodeKind

An enum with a string value, so `NodeKind.FUNCTION.value == "function"`.

```
PROJECT  MODULE  FILE  CLASS  FUNCTION  METHOD  VARIABLE
ATTRIBUTE  PARAMETER  TYPE_ALIAS  IMPORT  EXTERNAL_SYMBOL
DEPENDENCY  BOUNDARY
```

`NodeKind.from_value(value)` converts back from a string and raises
`ValueError` on an unknown one. `RelationKind` has the same pair.

## Relation

```python
Relation(source_id, target_id, kind, metadata=None)
```

## RelationKind

```
CONTAINS  DECLARES  IMPORTS  RESOLVES_TO  DEPENDS_ON
CALLS  REFERENCES  HAS_TYPE  INHERITS_FROM
EXPOSES  CONSUMES  COMMUNICATES_WITH
```

## Span

```python
Span(start_line, start_col, end_line, end_col)   # all 1-based
span.contains(line, col)
```

## ResolvedRef

What a resolver returns for one position.

```python
ResolvedRef(full_name, file_path, line, col, kind, origin)
```

`origin` is `internal`, `stdlib`, `third_party` or `unknown`. Only `internal`
sends the edge to a node in the graph; the rest produce an `EXTERNAL_SYMBOL`.

## OccurrenceRef

A use-site waiting to be resolved.

```python
OccurrenceRef(role, line, col, enclosing_id, span, detail=None)
```

`role` is `call`, `read`, `write`, `annotation`, `base` or `alias` (the
right-hand side of `x = y`, enclosed by the variable). `detail` carries what a
role cannot: `arg:LINE:COL:INDEX` on a `read` that is an argument of the call at
`LINE:COL`, and `return` / `result` on an `alias`. The flow pass reads them; a
custom resolver sees them as ordinary use-sites to answer.

## ResolverStatus

`OK`, `DEGRADED`, `UNAVAILABLE`. Stored in graph metadata as a string, and
merged to the worst of the two when graphs are combined.

`UNAVAILABLE` means resolution did not run or the backend could not answer;
`DEGRADED` means it ran and answered partially, which is the best PHP, C and C++
report. `YamlAdapter` reports `OK` with zero queries, because a YAML document has
no symbols to resolve — see [Resolvers](../adapters/resolvers.md).

## ResolverMetrics

```python
metrics.queries      # positions handed to the resolver
metrics.resolved     # answers that came back - a place, not necessarily a callee
metrics.internal     # bound to a node in the graph
metrics.external     # became an EXTERNAL_SYMBOL (outside the project, or not bound)
metrics.unresolved   # no answer
metrics.to_value     # calls that stopped at a variable, a parameter or a field
metrics.unlocated    # answers callix could not bind to a node
metrics.seconds
metrics.resolved_pct # share answered; not a measure of correctness
```

## GraphDiff

```python
diff.added_nodes, diff.removed_nodes, diff.changed_nodes
diff.added_relations, diff.removed_relations
diff.is_empty
```

`changed_nodes` holds `(old, new)` pairs.

## SpanIndex

Maps a source position back to a graph node. Built from a graph, queried during
resolution.

```python
index = SpanIndex.from_graph(graph)
index.at(file_path, line, col)          # by name span
index.enclosing(file_path, line, col)   # by full extent
```
