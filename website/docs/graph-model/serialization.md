---
sidebar_position: 4
---

# Serialization

```python
graph.to_dict()          # JSON-compatible dict
graph.to_json(indent=2)  # str

Graph.from_dict(data)
Graph.from_json(text)
```

Round-tripping is lossless, and the output is byte-compatible with graphlens —
a graph written by either one reads in the other.

## Shape

```json
{
  "schema_version": 1,
  "metadata": {
    "resolver_status": "ok",
    "resolver_metrics": {
      "queries": 28, "resolved": 25, "internal": 12,
      "external": 13, "unresolved": 3, "seconds": 0.034,
      "resolved_pct": 89.3
    }
  },
  "nodes": [
    {
      "id": "1f4a9c2b7e05d3a8",
      "kind": "function",
      "qualified_name": "app.services.charge",
      "name": "charge",
      "file_path": "app/services.py",
      "span": {"start_line": 12, "start_col": 1,
               "end_line": 20, "end_col": 14},
      "metadata": {}
    }
  ],
  "relations": [
    {
      "source_id": "1f4a9c2b7e05d3a8",
      "target_id": "9b3c7d1e2f80a465",
      "kind": "calls",
      "metadata": {"span": {"__span__": [15, 5, 15, 19]}}
    }
  ]
}
```

`ensure_schema_version` raises `SerializationError` on a version it does not
understand, rather than guessing.

## Spans inside metadata

Metadata is an open `dict[str, object]`, so a `Span` stored there needs a tag
to survive the round-trip:

```json
{"__span__": [start_line, start_col, end_line, end_col]}
```

Anything else is coerced to a JSON-compatible form, and whatever matches no
known shape becomes `str(value)`.

## Determinism

Two runs over unchanged source produce identical bytes. Node IDs are content-
derived, node order is insertion order, and JSON is emitted through the stdlib
`json` module with the same settings graphlens used — including
`ensure_ascii=False`.

This is what makes the diff useful:

```python
diff = old.diff(new)
diff.is_empty
```

and it is also the project's correctness harness: callix and graphlens are
compared by serializing both graphs with sorted keys and diffing the text.
