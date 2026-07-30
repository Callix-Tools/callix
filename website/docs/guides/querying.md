---
sidebar_position: 2
---

# Querying the graph

Everything below is a method on `Graph`. All of them take and return node IDs
or node objects — there is no query language to learn.

## By kind, file, or name

```python
from callix import NodeKind

graph.nodes_by_kind(NodeKind.FUNCTION)
graph.nodes_in_file("src/app/service.py")
graph.nodes_by_name("UserService")    # matches the short OR the qualified name
```

Paths are the project-relative ones the `FILE` nodes advertise, not absolute
ones — see [Nodes](../graph-model/nodes.md#paths-are-relative-always).

## Edges

```python
graph.outgoing(node_id)     # relations leaving the node
graph.incoming(node_id)     # relations entering it
```

## Call and reference navigation

```python
graph.callees(node_id)      # what it calls
graph.callers(node_id)      # what calls it
graph.references_to(node_id)
```

A worked example — everything that would be affected by changing a function:

```python
target = graph.nodes_by_name("charge_card")[0]

for caller in graph.callers(target.id):
    print(caller.qualified_name, caller.file_path)
```

## Neighbourhoods and subgraphs

```python
graph.neighbors(node_id, depth=2)   # distinct nodes within 2 hops, either way
graph.subgraph([id1, id2, ...])     # those nodes plus every incident edge
graph.subgraph_for_file("src/app/service.py")
```

`subgraph()` returns a real `Graph`, so it serializes and diffs like any other.
Its nodes keep the parent's insertion order rather than the order of the ids you
passed, so the result is byte-stable and usable as a cache key or a golden file.

## Across services

```python
graph.merge(other, allow_shared=True)   # one graph per language, combined
graph.link_boundaries()                 # -> int, the COMMUNICATES_WITH edges added
```

See [Cross-language analysis](./cross-language.md).

## Metadata

Node metadata carries whatever the adapter learned that does not fit the
schema:

```python
node.metadata["origin"]       # on IMPORT / EXTERNAL_SYMBOL
node.metadata["import_path"]  # on IMPORT
node.metadata["name_span"]    # the identifier's span, used by resolution
```

Relation metadata carries the site:

```python
relation.metadata["span"]     # where the call or reference is written
relation.metadata["access"]   # 'read' | 'write', on REFERENCES
```
