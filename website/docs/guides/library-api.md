---
sidebar_position: 1
---

# Library API

## Building a graph

```python
from callix import PythonAdapter

adapter = PythonAdapter()
graph = adapter.analyze("path/to/project")
```

`analyze()` accepts an explicit file list, which skips discovery:

```python
files = adapter.collect_files(root)
graph = adapter.analyze(root, files=[f for f in files if "tests" not in f.parts])
```

And a strict mode, which refuses to hand back a partial result:

```python
graph = adapter.analyze(root, strict=True)   # AdapterError unless status is 'ok'
```

## Adapter options

Every adapter takes `resolve`:

```python
PythonAdapter(resolve=False)        # structure only
TypeScriptAdapter(resolve=False)
GoAdapter(resolve=False)
RustAdapter(resolve=False)
```

`PythonAdapter` additionally takes replacements for its two pluggable parts:

```python
PythonAdapter(resolver=my_resolver)          # a custom symbol resolver
PythonAdapter(dep_parsers=[my_parser])       # custom manifest parsers
```

See [Custom resolvers](./custom-resolvers.md) for the protocols.

## Merging graphs

Analysing a polyglot repository means one graph per language, combined:

```python
from callix import GoAdapter, PythonAdapter

graph = PythonAdapter().analyze(root)
graph.merge(GoAdapter().analyze(root), allow_shared=True)
```

`allow_shared=True` permits *identical* nodes to coincide — which is exactly
what happens at a boundary, where two adapters deliberately produce the same
`BOUNDARY` node. A collision between two *different* nodes with one ID stays an
error even then.

Resolver statuses merge to the worst of the two, so a degraded half cannot hide
behind a healthy one.

## Serialization

```python
data = graph.to_dict()          # JSON-compatible dict
text = graph.to_json(indent=2)

graph = Graph.from_dict(data)
graph = Graph.from_json(text)
```

Round-tripping is lossless, spans included. See
[Serialization](../graph-model/serialization.md) for the schema.

## Diffing

```python
diff = old.diff(new)
diff.is_empty
diff.added_nodes, diff.removed_nodes, diff.changed_nodes
diff.added_relations, diff.removed_relations
```

Ordering inside each list is deterministic: nodes by ID, relations by their
key. Edge metadata is part of that key, so two `CALLS` between the same pair
from different call sites stay distinct, and a change to an edge's metadata
alone still shows up.
