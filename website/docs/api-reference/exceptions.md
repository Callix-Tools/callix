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
| `AdapterError` | `analyze(strict=True)` with a resolver status other than `ok` |

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
