---
sidebar_position: 4
---

# Custom resolvers and parsers

Analysis moved to Rust, but the pluggable points did not disappear: all three
are called back through the ordinary Python protocol, so a plain Python object
still works.

Every adapter takes the same four keyword-only arguments, and they mean the same
thing in each:

```python
Adapter(
    resolve=True,               # False: skip resolution, keep the structure
    resolver=None,              # replaces the native backend
    dep_parsers=None,           # replaces the built-in manifest reader
    boundary_extractors=None,   # runs in addition to the built-in ones
)
```

`YamlAdapter` is the one exception, and takes `resolve` and
`boundary_extractors` only — [see below](#why-yaml-is-different).

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

Three properties of the protocol are worth knowing before you write one:

- **A custom resolver replaces the native one; it is not consulted after it.**
  There is no fallback chain to reason about, and no way for two backends to
  answer the same use-site differently.
- **`resolve_all` must answer every query.** Answers are matched to queries by
  index, so returning a shorter list would shift every later answer onto the
  wrong use-site. Callix raises `AdapterError` rather than accept that; use
  `None` for the ones you do not know.
- **`resolve=False` wins.** Passing both `resolve=False` and a resolver leaves
  the graph structural and never calls the resolver, because that combination is
  almost always someone switching resolution off to time the structural phase.

The status you report is the one recorded in `graph.metadata["resolver_status"]`,
and it is what `analyze(..., strict=True)` checks — so a resolver that reports
`ok` when it is guessing defeats the one guard a caller has.

## The bundled ty resolver

`callix.TyResolver` wraps the embedded ty and implements the same protocol, so
it can be subclassed or replaced piecemeal:

```python
from callix import TyResolver

resolver = TyResolver()
resolver.prepare("path/to/project", [])
resolver.definition_at("src/app.py", 12, 5)
```

## A boundary extractor

Boundary detection is an open-ended surface — new frameworks appear faster than
any built-in list can follow — so it is pluggable too. An extractor is an object
with one method:

```python
class DjangoUrls:
    def extract(self, source: bytes, file_path: str) -> list[BoundaryRef]:
        ...
```

It receives the file's bytes and its project-relative path, and decides for
itself how to read them. That is deliberately unlike graphlens, which handed
extractors a tree-sitter node: a Rust CST node cannot cross into Python, and a
mechanism expressed by *file layout* rather than by a call — Django's
`urlpatterns`, Next.js route files, an OpenAPI document — never wanted a node
in the first place.

```python
import re
from callix import BoundaryRef, PythonAdapter, normalize_http_path

class DjangoUrls:
    PATTERN = re.compile(rb'path\(\s*["\']([^"\']*)["\']')

    def extract(self, source, file_path):
        if not file_path.endswith("urls.py"):
            return []
        found = []
        for match in self.PATTERN.finditer(source):
            route = normalize_http_path("/" + match.group(1).decode())
            found.append(BoundaryRef(
                mechanism="http",
                role="server",
                key=f"GET {route}",
                line=source[: match.start()].count(b"\n") + 1,
                col=1,
                confidence=0.8,
                detail={"method": "GET", "path": route, "framework": "django"},
            ))
        return found

graph = PythonAdapter(boundary_extractors=[DjangoUrls()]).analyze(root)
# http:GET /users/{}   ·   http:GET /health
```

Two things matter for the key to be useful:

- **Normalize it yourself.** `normalize_http_path` is exported and understands
  every parameter style, Django's `<int:uid>` included. Skip it and your route
  will not meet the client that calls it, because matching is by exact key.
- **Set an honest confidence.** A route read from a literal deserves a high
  one; anything guessed does not.

Extractors run **in addition** to the built-in ones, so adding Django support
does not mean reimplementing FastAPI. Anything they raise propagates — a broken
extractor should be noticed, not quietly yield an empty graph.

Every adapter accepts the argument.

## A dependency parser

Manifest parsing is pluggable the same way — an object with `can_parse` and
`parse`:

```python
class LockfileParser:
    def can_parse(self, project_root):
        return (project_root / "my.lock").is_file()

    def parse(self, project_root):
        return ["requests", "httpx"]

graph = PythonAdapter(dep_parsers=[LockfileParser()]).analyze(root)
```

Any iterable of names will do. They are what import classification compares
against, so they decide whether an import is `third_party` or `unknown`, and they
are also what becomes the graph's DEPENDENCY nodes.

Custom parsers **replace** the built-in reader rather than adding to it, so a
parser that only understands your lockfile will hide `pyproject.toml`. Every
built-in reader is exported — `parse_dependencies`, `ts_parse_dependencies`,
`go_parse_dependencies`, `rust_parse_dependencies`, `php_parse_dependencies`,
`c_parse_dependencies` — so a parser that wants to add rather than replace can
call one and return the union.

What a name means is the language's business: a distribution name in Python, a
package name in TypeScript, a module path in Go, a crate in Rust, a vendor prefix
in PHP, a library in the C family. A parser is written for one language, not for
all of them.

## Which backend each adapter replaces

| Adapter | `resolver=` replaces | `dep_parsers=` replaces |
|---|---|---|
| `PythonAdapter` | the embedded ty | pyproject.toml, requirements*.txt, setup.cfg |
| `TypeScriptAdapter` | the embedded typescript-go | package.json |
| `GoAdapter` | `go/packages` + `go/types` | go.mod |
| `RustAdapter` | the `rust-analyzer scip` index | Cargo.toml |
| `PhpAdapter` | the built-in symbol table | composer.json |
| `CAdapter`, `CppAdapter` | the built-in symbol table | CMake, vcpkg, conan, meson |

The C family is the one place where a custom resolver is asked a *different*
question than the built-in backend. The shipped symbol table resolves by name and
`#include` visibility, because that is what C and C++ actually do and because no
position-keyed index can be built without a `compile_commands.json`. A custom
resolver is asked about positions like every other adapter's — which is exactly
the shape a `scip-clang` or clangd index has, and
[`ClangScipResolver`](../adapters/c-family.md#clangscipresolver-the-real-thing-if-you-have-a-compdb)
ships in the module as exactly such a resolver, wired to `scip-clang`, for
projects that have a compilation database. See
[C and C++](../adapters/c-family.md#resolution-and-why-it-is-degraded).

## Why YAML is different

`YamlAdapter` takes `resolve` and `boundary_extractors` only. YAML declares no
symbols, so there is no resolution phase to redirect and no use-site a resolver
could be asked about; its dependencies come from a Helm `Chart.yaml` wherever one
is found, not from a manifest at the project root, so the `can_parse(root)` /
`parse(root)` protocol has nothing to bind to.

Accepting the two arguments and ignoring them would be worse than refusing them:
a caller who passed a resolver would be told nothing while it was never called.
Passing either raises `TypeError`.
