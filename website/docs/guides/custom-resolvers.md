---
sidebar_position: 4
---

# Custom resolvers and parsers

Analysis moved to Rust, but the two pluggable points did not disappear: both
are called back through the ordinary Python protocol, so a plain Python object
still works.

## A resolver

Any object with these three methods:

```python
class MyResolver:
    def prepare(self, project_root, files):
        """Called once before resolution. Build whatever index you need."""

    def resolve_all(self, queries):
        """queries: list[tuple[Path, int, int]] — file, 1-based line, column.

        Return a list of the same length; None where nothing was found.
        """
        return [None] * len(queries)

    def status(self):
        """'ok' | 'degraded' | 'unavailable' — recorded in graph metadata."""
        return "ok"
```

Each answer is either `None` or an object with `full_name`, `file_path`,
`line`, `col`, `kind` and `origin`. The built-in `ResolvedRef` is exported for
convenience:

```python
from callix import PythonAdapter, ResolvedRef

class ConstantResolver:
    def prepare(self, project_root, files): ...
    def status(self): return "degraded"
    def resolve_all(self, queries):
        return [
            ResolvedRef(full_name="", file_path=None, line=0, col=0,
                        kind="", origin="unknown")
            for _ in queries
        ]

graph = PythonAdapter(resolver=ConstantResolver()).analyze(root)
```

`origin` decides what the edge points at. `internal` means "look this position
up in the graph"; anything else sends the edge to an `EXTERNAL_SYMBOL`.

## The bundled ty resolver

`callix.TyResolver` wraps the embedded ty and implements the same protocol, so
it can be subclassed or replaced piecemeal:

```python
from callix import TyResolver

resolver = TyResolver()
resolver.prepare("path/to/project", [])
resolver.definition_at("src/app.py", 12, 5)
```

## A dependency parser

Manifest parsing is pluggable the same way — an object with `can_parse` and
`parse`:

```python
class LockfileParser:
    def can_parse(self, project_root):
        return (project_root / "my.lock").is_file()

    def parse(self, project_root):
        return frozenset({"requests", "httpx"})

graph = PythonAdapter(dep_parsers=[LockfileParser()]).analyze(root)
```

The names returned here are what import classification compares against, so
they decide whether an import is `third_party` or `unknown`.

:::note
`resolver` and `dep_parsers` are currently accepted by `PythonAdapter` only.
The other three adapters take `resolve=False` to skip resolution, but their
backends are not swappable from Python yet.
:::
