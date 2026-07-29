---
sidebar_position: 3
---

# Python adapter

```python
from callix import PythonAdapter

graph = PythonAdapter().analyze("path/to/project")
PythonAdapter(resolve=False).analyze(root)      # structure only
```

| Property | Value |
|---|---|
| Language id | `python` |
| Project markers | `pyproject.toml` (with a `[project]` section), `setup.cfg`, `setup.py` |
| Extensions | `.py`, `.pyi` |
| Resolver | embedded ty |
| External requirements | none |

## What it extracts

Modules as dotted names, classes, functions and methods, module- and
class-level variables, attributes assigned through `self`, type aliases,
parameters, and imports — absolute and relative alike, with `from . import x`
resolved against the importing module's package.

Occurrences are recorded for calls, identifier reads and writes, type
annotations, and base classes; resolution turns them into `CALLS`,
`REFERENCES`, `HAS_TYPE` and `INHERITS_FROM`.

## Project detection

A `pyproject.toml` counts only when it has a `[project]` section — otherwise a
Rust project keeping one for tooling would be mistaken for a Python project.
Failing every marker, the adapter falls back to "is there any `.py` here",
which is what makes Python subpackages inside a polyglot monorepo visible.

## Dependency classification

Third-party names come from `pyproject.toml` (PEP 621 and Poetry),
`requirements*.txt` (with one level of `-r` expansion) and `setup.cfg`.
Dev and test groups are included on purpose: without them, imports that appear
only in tests would classify as `unknown` rather than `third_party`.

Standard-library names come from `sys.stdlib_module_names` of the running
interpreter — a property of the Python you are using, not of the build, and not
of the project being analysed. A module added to the standard library after your
interpreter was released classifies as `unknown` rather than `stdlib`.

## Boundaries

FastAPI, Flask and Starlette routes; requests, httpx and session calls; gRPC
servicer classes and stubs; Temporal and DBOS activities; publish / subscribe.

## The embedded ty

The resolver calls ty as a library rather than spawning `ty server`, which
removes JSON-RPC and serialization from the hot path. Positions are counted in
UTF-16, matching the LSP default the Python client used to negotiate.

One known divergence from `ty server`: `goto_definition` occasionally returns a
file inside the bundled typeshed where the server would give a path in the
project, and that occurrence falls through to an `EXTERNAL_SYMBOL`. The cause
is that ty server marks files as open while a directly instantiated database
does not. Measured on apache/superset: 22 nodes out of 225,706 — 0.01% — with
edges and the resolved share matching exactly.
