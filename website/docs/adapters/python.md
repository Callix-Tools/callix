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
class-level variables, attributes assigned through `self` or `cls` (one `ATTRIBUTE` of the class, however many methods assign it), type aliases,
parameters, and imports — absolute and relative alike, with `from . import x`
resolved against the importing module's package, wherever they are written: in a
function body, or under `if TYPE_CHECKING:` or `try:`.

A `lambda` is a function nobody named: a `FUNCTION` node called `<lambdaN>` in the
scope it is written in, numbered in the order that scope meets them
(`app.<lambda1>`, `app.make.<lambda2>`), with `metadata["lambda"] = true`. Its
parameters are `PARAMETER` nodes and its body belongs to it, so what a lambda
calls is its own edge. A lambda that is assigned, passed as an argument or
returned is followed to where it is called (`resolution: "flow"`).

`obj.x = v` declares no local: it reads `obj` and writes the attribute `x`, which
resolves to the class that defines it. Every target of a chained assignment
(`a = b = v`) is declared. At module level the targets of `for` and
`except ... as` are module variables, because other modules can import them.

Inside a function, a loop variable is a `VARIABLE` node only when the body of the loop calls it
(`for f in handlers: f()`): a node per loop counter would bury the graph. That
call is followed to what the iterable hands out: the elements of a container, what
a generator `yield`s (read like a `return`), what an iterator's `__next__` returns.

Beyond ty, a flow pass follows a function carried in a value (see
[Accuracy](../project/accuracy.md#what-flow-is)): decorators, receivers,
containers read by key, unpacking, parameter defaults, call results and a closed
list of library functions that call an argument (`map`, `filter`, `sorted(key=)`,
…). Every such edge is marked `resolution: "flow"`, with the holder it went
through in `via`.

Occurrences are recorded for calls, identifier reads and writes, type
annotations, and base classes; resolution turns them into `CALLS`,
`REFERENCES`, `HAS_TYPE` and `INHERITS_FROM`.

Every name in an annotation is a use, not only the first: `list[Item]`,
`Optional["Item"]`, `dict[str, Item] | None`, `Callable[[A], R]`, a quoted forward
reference, and annotated variables (module, class field, local, `self.x: T`) all
produce a `HAS_TYPE` from the parameter, function, variable or attribute. In a
qualified name such as `models.Item` the type is the last name: the `HAS_TYPE`
goes to the class and `models` is a `REFERENCES` to the module. `Literal[...]`
arguments and the metadata of `Annotated[T, ...]` are values, not types, and a
string annotation that does not parse as a single expression is ignored.

The name of a keyword argument in a call references the project field it sets
(`Item(name=...)` on a dataclass, a `NamedTuple`, a `TypedDict`, or a
`dataclass_transform` model such as a pydantic one), with `access: "read"` and
`resolution: "definition"`. The keywords of an ordinary function or of a
hand-written `__init__`, `**kwargs` and unknown names produce no edge.

## Projects inside the directory

A directory can hold Python projects of its own - `repo/backend` with a
`pyproject.toml` and a `src/` layout is the usual one - and each is imported from
its own source root (`repo/backend/src`), which is not the directory analysed.
callix hands those roots to the type checker, so `from service.group_member import
create` resolves whether `repo` or `repo/backend` is passed. Node names follow the
source root: `backend/src/api/group_member.py` is the module `api.group_member`.

`from pkg import name` brings in what `pkg/__init__.py` binds to `name` when it
binds one - the function of a submodule of the same name, for instance - and not
the submodule. The import then depends on the package, and the call is an edge to
the function.

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
