---
sidebar_position: 8
---

# C and C++

```python
from callix import CAdapter, CppAdapter

c   = CAdapter().analyze("path/to/project")
cpp = CppAdapter().analyze("path/to/project")
```

Two adapters, one implementation. A repository may be a C project, a C++
project, or legitimately both, so they are separate classes on the Python side —
but everything hard about the family is shared, and a second visitor would be
the first one with the `namespace` and `class` arms deleted, and would drift.

Analyse a mixed repository twice and merge. Every file, including every
ambiguous `.h`, is claimed by exactly one of the two adapters, so the merge
cannot collide.

## The header problem

A header is textually included into many translation units. callix's model says
a FILE node contains the declarations found in it, and that assumption does not
survive contact with `#include`. The resolution:

**One FILE node per file, headers included.** A header is parsed once, on its
own — not once per includer. `#include` becomes an IMPORT node with an IMPORTS
edge from the including file and a RESOLVES_TO edge to the header's FILE node
when the path resolves inside the project.

**A prototype and its definition are ONE node.** External-linkage qualified
names carry no file component, so `engine_build` declared in `engine.h` and
defined in `engine.c` deliberately collapse. They are one entity, and a graph
that split them would answer "who calls this?" twice.

**The definition wins.** Which site the node is built from is decided in a
survey pass over every file *before* the first node is written. Otherwise
whichever file happened to be read first would set the node's span — a
determinism bug, not a cosmetic one. The other declaring sites are recorded in
`metadata["declared_in"]`, and every one of them emits a DECLARES edge. Where
each one writes the name is `metadata["declared_at"]`, one `{file, span}` entry
per site (a prototype in the same file as the definition is listed there, not in
`declared_in`):

```
engine_build   file_path=src/engine.c:12   declared_in=['include/engine.h']
               declared_at=[{'file': 'include/engine.h', 'span': 12:5-12:17}]
```

An `#include` is positioned too: the path literal, quotes or angle brackets
included, is the IMPORT's `name_span` and the `span` of its RESOLVES_TO to the
header's FILE node (or to an external symbol). A computed `#include MACRO`
names no file and is not positioned.

**File-local names are prefixed with their file.** A `static` function in C, and
anything in a C++ anonymous namespace, is visible only inside its translation
unit, so two files may legitimately define `static const char *label_for(...)`
and they are different functions:

```
src/engine.c::label_for   linkage=internal
src/main.c::label_for     linkage=internal
```

Getting that backwards merges two unrelated functions — and the idiom is
everywhere in C.

`metadata["linkage"]` is `internal` or `external` on every function and
file-scope variable.

## Qualified names

C has no namespaces, so an external-linkage name is bare: `engine_build`.
Struct, union and enum tags live in their own namespace in C, which is why the
node kind is part of the id — `struct stat` and `stat()` coexist in every libc.

A struct or union named only by a typedef (`typedef struct { .. } T;`) is a
CLASS `T` beside the TYPE_ALIAS `T` — the kind is part of the id, so both
exist — and its fields are `T::field`. An anonymous struct or union MEMBER of a
named record lends its fields to that record (C11), so `s.a` in
`struct s { union { int a; float f; }; };` binds to `s::a`.

C++ joins namespaces and classes with `::`, and **a callable carries its
normalized parameter list**, because two overloads would otherwise collide on
one id:

```
fixture::Engine::ping() const        061c4f7b9a88bd07
fixture::Engine::ping(int) const     cf0798fb4bb51b4a
```

Parameter names are stripped and whitespace collapsed; cv-qualifiers and
reference and pointer markers are kept, since they distinguish real overloads.
C does not do this — it has no overloading, and adding a parameter list would
gratuitously make `engine_build()` harder to look up than it needs to be.

Out-of-line definitions are classified with a workspace-wide pass. The grammar
reports `qualified_identifier.scope` as a namespace identifier even when the
scope is a class, and the class is usually declared in a different file, so the
set of class names is collected across every file of the root before anything
decides whether `X::y` is a method or a namespaced function.

## MODULE is the directory

In both dialects. A C++ namespace groups declarations but it does not *contain*
files — it spans them — and in every other adapter a MODULE is what a FILE nests
under. Emitting both shapes put `fixture` beside `include` and `src` in one
graph with no way to tell which kind of MODULE you were looking at.

Namespaces are not lost: they are in every qualified name the visitor builds,
which is where a scope that owns no file belongs.

Neither shape is a C++20 `module`. Nothing here reads `import std;`, and a
project using named modules is analysed through its `#include`s like any other.

## Resolution, and why it is `degraded`

Every precise option for this family — [scip-clang](https://github.com/sourcegraph/scip-clang),
clangd's index, libclang — hard-requires a `compile_commands.json`. That file
cannot be derived from sources; it is a by-product of running the build. A
survey of twelve of the most prominent C and C++ repositories found it checked
in **none** of them, and both redis and the Linux kernel gitignore it outright.

So a compdb-requiring resolver would report `unavailable` on very nearly every
folder a user points callix at. That is a worse failure than Go's or Rust's "use
the toolchain a project of that language already has", where the requirement is
something the project necessarily owns.

The shipped default is therefore a symbol table over the sources callix already
parsed, reporting `degraded`. It is not a placeholder: C and C++ resolve names
by declaration visibility, and a header graph encodes exactly that. A call to an
external-linkage name binds anywhere in the workspace it is reachable by
`#include`; a call to a file-local name binds only within its own file, which is
what `static` means. Include reachability is closed transitively, so a two-hop
include chain resolves — iteratively rather than recursively, because a cyclic
include is legal and was common before `#pragma once`.

As in every other adapter, each edge the table produces records where it was
written: `metadata["span"]`, plus `access` on a `REFERENCES`. Two calls to one
function are therefore two edges, and an index built from the graph — an LSIF
dump, say — can place every reference.

A call is not bound by its name alone. A call site carries what its syntax says
about which function it means, and the table reads it in the order C++ does:

- `obj.f()` / `obj->f()` / `this->f()`: the **declared type of the receiver** — a
  parameter, a local, an `auto` initialised by `new T` or `T(...)`, `this`, or a
  field of the enclosing class — decides, and the nearest class in its lineage
  that has a member `f` wins, hiding the rest as C++ name lookup does. A receiver
  whose type is a library class (`std::string`) is not a project call at all.
- `x->f` / `x.f` read or written, and `f` in a member initialiser list: the same
  receiver rule, applied to **fields**. The receiver may be a parameter, a local,
  `this`, a file-scope variable, a field chain (`n->next->len`), a cast, or a
  typedef'd pointer (`srv_t s`); the field is looked up in the receiver's class
  and its bases. A receiver whose type is known and has no such field (`FILE *fp`,
  `fp->_flags`) is an external symbol, not a guess. An unqualified name never binds
  to a field, so `mstime()` is the function even when a struct has a field
  `mstime`.
  A typedef of an inline-defined tagged struct (`typedef struct tag { .. } T`)
  leads to `tag`.
- `A::f()` / `n::f()`: a written scope names a class's member or a namespace's
  function.
- `f()` inside a method means the enclosing class's `f` first.
- `T x;`, `T x(1);`, `new T(1)`, and a base class in an initialiser list are
  calls of a **constructor**, picked by the number of arguments and how they are
  spelled; a class with no constructor of its own is instantiated as the class.
- Overloads are told apart by parameter count, then by how each argument is
  spelled (`1`, `1.0`, `"s"`, `'c'`, `true`, a local of a known type) against the
  parameter type. A tie takes the first declaration and the edge says
  `ambiguous`.
- A **type name** is bound by name to a class or a typedef, never to a function or
  variable of the same name. A type spelled in a body (a local declaration, a cast,
  `sizeof`/`alignof`, a compound literal, a template argument, a lambda, `catch` or
  function-pointer parameter) and each class in a written scope (`Impl::Get`) is a
  `REFERENCES` from the enclosing function to the `CLASS` or `TYPE_ALIAS`. The type
  of a declared node, its template arguments and its function-pointer signature
  included, is its `HAS_TYPE`. An unadorned name prefers the typedef, and
  `struct S` prefers the struct. A namespace, `std::string`-style library scopes and
  `sizeof(variable)` write nothing; a library type written in a body becomes an
  external symbol, except a construction of a library class, which writes nothing.

Every edge the table writes has `metadata["resolution"] == "name"`, because that
is how it was found, and `"ambiguous": true` when it had to choose between
declarations it could not tell apart. A receiver whose type nothing in reach
declares falls back to the bare name — a method or function-pointer field for a call, a field for a read or write — the one case where `a.run()` or `a->len` may bind to the wrong class, and the one the edge flags.

What it will not do:

- **Evaluate the preprocessor.** Every arm of an `#if` is in the graph, so two
  mutually exclusive definitions of one function both exist. A compiler-based
  indexer sees one. This way nothing is invisible because of a flag nobody
  recorded — but it does mean the graph can hold code that never compiles
  together.
- **Run the preprocessor.** A macro *of the project* is expanded where it is
  invoked: `CALL(x)` is a call of what `CALL`'s body calls (a parameter called as a
  function is the argument that was passed; `##` is joined; a macro within a macro is
  followed), a macro invoked where a declaration goes declares what it expands to,
  and the macro's own name is not a call. A macro of the project that expands to nothing, an attribute or a storage keyword (`class API Foo`, `int x GUARDED_BY(m)`, `API int (*hook)(int)`) is erased before parsing, so the declaration is read as written; this includes
  macros defined in a header the other dialect reads (a macro-only `.h`, which probes
  as C). A storage-keyword macro whose name is shorter than its keyword, or one defined
  differently in another `#if` arm, is not erased, and neither is a macro from another
  root or a system header. Macros from system headers are not known,
  conditionals are not evaluated (every definition of a macro is followed), and a body
  that does not parse on its own is skipped.
- **Resolve overloads exactly.** Choosing the overload a call selects needs the
  argument types, which needs a type checker; the table scores how the arguments
  are spelled and says `ambiguous` when that does not decide.
- **Follow every function value.** A function pointer, a `std::function` or a table
  of callbacks is followed through assignments, initialisers, arguments and returns
  (`flow` edges). A callback handed to a library function is a call only for a
  closed list of them (`qsort`, `signal`, `std::sort`, `std::thread`, …). A function
  that returns a function (`pick()(x)`) is followed.
- **Resolve templates.** A name that depends on a template parameter is unknown
  until instantiation. An overloaded operator, a functor and `delete` are bound
  by the declared type of the variable they are applied to (an object, not a
  pointer, for an operator), so only when that type is visible.

Because the status is never `ok`, `strict=True` always raises for this family.
That is deliberate: `strict` means "refuse to hand back a graph you cannot
vouch for", and here callix cannot.

A compiler-backed backend plugs in through `resolver=`, like every other
adapter's:

```python
CppAdapter(resolver=MyScipIndex()).analyze(root)
```

With one difference worth knowing. The built-in table resolves **by name** and
`#include` visibility, because that is what the language does and because no
position-keyed index exists without a compdb. A custom resolver is asked the
question every other adapter asks its backend — "what is defined at this
position?" — which is exactly the shape a `scip-clang` or clangd index has, and
`src/rustlang/scip.rs` already holds a decoder that does not care which indexer
produced the index.

It has to be selected explicitly rather than detected, so that the same folder
cannot yield structurally different graphs depending on whether `cmake` happened
to have run.

## Include guards

Worth knowing, because it is where a naive implementation reads zero:
the body of `#ifdef` / `#ifndef` / `#if` is a set of **unnamed children** in the
grammar, not a `body` field. Essentially every real header is wrapped in an
include guard, so a visitor that walks only the translation unit's own children
finds nothing at all in it. The walk descends through the conditional arms —
and through `extern "C" { }` and `template<...>` — transparently, inside struct
and enum bodies as well as at the top level.

## Dependencies

There is no universal manifest, so this is best-effort and says so: CMake
`find_package` / `target_link_libraries` / `FetchContent_Declare`, `vcpkg.json`,
`conanfile.txt` and `conanfile.py`, and meson `dependency()`. A system header
included with no manifest entry is invisible, and pretending otherwise would be
dishonest.

## Boundaries

| Mechanism | Recognised |
|---|---|
| HTTP client | libcurl `curl_easy_setopt(h, CURLOPT_URL, ...)` — including a URL built by `snprintf` or `+`, whose variable part normalizes to `{}` |
| HTTP server | civetweb `mg_set_request_handler`, microhttpd `MHD_start_daemon`; C++ cpp-httplib `svr.Get(...)`, Crow `CROW_ROUTE`, Drogon `ADD_METHOD_TO` |
| gRPC | a class inheriting `X::Service` is a server, an `X::NewStub` call is a client |

A format string is reduced whole, not by its conversion character:
`"/engines/%ld"` becomes `/engines/{}` — stopping at the first letter would
leave the `d` of `%ld` behind and the key would never meet a route. That is the
point of the exercise: the fixture's C and C++ clients produce the same key as
an OpenAPI `GET /engines/{id}`, a Python `httpx.get`, a PHP Guzzle call and a
TypeScript `fetch`, so all five meet in one BOUNDARY node.
