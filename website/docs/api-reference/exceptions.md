---
sidebar_position: 3
---

# Exceptions

```
CallixError
├── ParseError
│   └── ResolverTimeout
├── DuplicateNodeError
├── SerializationError
└── AdapterError
```

| Exception | Raised when |
|---|---|
| `CallixError` | base class — catch this to catch everything |
| `ParseError` | a source file could not be parsed |
| `ResolverTimeout` | a resolver exceeded its time budget |
| `DuplicateNodeError` | `add_node` hit an existing ID, or `merge` found two different nodes sharing one |
| `SerializationError` | an unsupported schema version, or malformed input |
| `AdapterError` | `analyze(strict=True)` with a resolver status other than `ok`, or a custom resolver that broke its protocol |

`SerializationError` is the *only* thing reading a payload raises: malformed
JSON, a missing key and an unknown kind all funnel through it, so a caller has
one type to catch rather than three.

A custom resolver that returns fewer answers than it was given queries also
raises `AdapterError`. Answers are matched to queries by index, so a short list
would silently attach every later answer to the wrong use-site — see
[Custom resolvers and parsers](../guides/custom-resolvers.md).

```python
from callix import CallixError, PythonAdapter

try:
    graph = PythonAdapter().analyze(root, strict=True)
except CallixError as exc:
    print(exc)
```

Note what is **not** an exception: a missing toolchain. Without Go or
`rust-analyzer` the adapter returns a structural graph and records
`resolver_status`. Use `strict=True` when you would rather it failed.
