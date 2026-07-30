---
sidebar_position: 7
---

# PHP

```python
from callix import PhpAdapter

graph = PhpAdapter().analyze("path/to/project")
```

PHP is the first language callix analyses without a type checker behind it.
There is no PHP equivalent of `ty` to link into the wheel, and the one credible
indexer — Intelephense — is a Node.js language server, which is a dependency
callix will not take. So resolution is a symbol table built from the sources
themselves, and it reports `degraded` rather than `ok`. That is not a
placeholder; see [what it cannot answer](#what-it-cannot-answer).

## Project detection

`composer.json` is the marker, and its `autoload` section is what makes
namespaces resolvable: the PSR-4 map turns a namespace prefix into a directory.
Both `psr-4` and `psr-0` are read, from `autoload` and `autoload-dev`, and
prefixes are ordered longest-first so `App\Domain\` wins over `App\`.

A great deal of PHP ships without a manifest, so detection falls back to "any
`.php` file exists". A project found that way still produces a full structural
graph; only the namespace-to-directory mapping is missing.

`vendor/` is excluded, and only at the project root — that is where Composer
writes it, and a mid-sized Symfony application vendors more PHP than it
contains. `var/`, `cache/` and `.phpunit.cache/` are excluded at any depth.

## The node scheme

The separator is a **backslash at every level**. There is no `::` anywhere in a
PHP qualified name, including for methods:

```
module     Fixture\Models
class      Fixture\Models\Engine
method     Fixture\Models\Engine\ping
parameter  Fixture\Models\Engine\__construct\region
attribute  Fixture\Models\Engine\region
```

A `MODULE` is a **namespace**, not a file — one of five different meanings of
MODULE across callix's adapters (Python and TypeScript use a dotted path, Go a
package directory, Rust a module path, C and C++ the directory). A class binds to
the MODULE for its longest namespace prefix. The full table is in
[Nodes](../graph-model/nodes.md).

`CLASS` covers class, interface, trait **and** enum, distinguished by metadata
booleans `is_interface`, `is_trait`, `is_enum` and `is_abstract`. PHP's four
declaration forms behave alike enough in a call graph that four node kinds
would buy nothing, and the graph vocabulary is fixed by
[the versioning contract](../project/versioning.md).

Two things share a qualified name and are still distinct nodes, because the kind
is part of the id: a promoted constructor property and a same-named getter. In
the fixture, `Fixture\Models\Engine\region` exists both as an `attribute` (from
`private Region $region`) and as a `method` (from `region(): Region`).

A promoted constructor parameter produces **both** a `parameter` and an
`attribute` node, because it genuinely is both.

## What resolution does

The symbol table resolves a use-site through, in order: the file's `use` alias
map, the enclosing namespace, the global namespace, then a PSR-4 prefix lookup.
Receiver types are inferred for the cases that are syntactically recoverable and
overwhelmingly common — `$x = new Foo()` followed by `$x->m()`, promoted
constructor properties, `$this->`, and `self::` / `static::` / `parent::`.

Trait methods resolve through the class that uses them. In the fixture,
`inspect(Base $item)` calling `$item->describe()` binds to
`Fixture\Models\Describes\describe` — the trait's method, which is where the
implementation actually is.

Using a trait produces an `INHERITS_FROM` edge, the same as `extends` and
`implements`. PHP traits are mixed in rather than inherited, so this is a
modelling choice: the graph records that the methods arrive from there, which is
the question a call graph is asked.

## What it cannot answer

Stated plainly, because `degraded` is the honest status and not a formality:

- **`$var->method()` where `$var`'s type comes from anywhere else** — a function
  return, an array element, a parameter with no type declaration.
- **Dynamic class names** — `$cls::method()`, `new $cls`.
- **`__call` and `__get`** — magic methods are invisible; nothing declares what
  they accept.
- **Anything in `vendor/`** — deliberately not collected, so a call into a
  framework becomes an `EXTERNAL_SYMBOL`. That is the right answer for a call
  graph of *your* code, but it means callix cannot tell you what
  `$container->get('service')` returns.

Everything unresolved still produces an edge to an `EXTERNAL_SYMBOL`, so a
reference is never silently dropped.

## Boundaries

| Mechanism | Recognised |
|---|---|
| HTTP server | Laravel `Route::get`/`post`/`put`/`patch`/`delete`/`any`/`match`, Symfony `#[Route(...)]`, Slim `$app->get(...)` |
| HTTP client | Guzzle `$client->get($url)` and `->request('GET', $url)`, `file_get_contents` on a URL, `curl_setopt($h, CURLOPT_URL, ...)` |
| Queue | Laravel `Queue::push`, `dispatch(new Job)`, Redis publish |

Interpolated paths normalize the same way as every other language, which is the
whole point: `"/engines/{$ident}"` becomes the key `GET /engines/{}`, identical
to what an OpenAPI `GET /engines/{id}` and a Python `httpx.get(f"/engines/{id}")`
produce. Merge the three graphs and all three meet in one BOUNDARY node —
see [Cross-language analysis](../guides/cross-language.md).

One detail worth knowing when reading a Symfony codebase: the `#[Route]`
attribute's arguments sit under the grammar's `parameters` field rather than
`arguments`, which is why an extractor written against the obvious field name
finds nothing.

## Files with no PHP in them

A `.php` file that is pure HTML parses to a single text node. That is
legitimate for a template, so it contributes a FILE node and nothing else rather
than counting as a parse failure.
