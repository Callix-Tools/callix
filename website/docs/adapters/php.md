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

`vendor/` is excluded at the project root and in the tree of any nested
Composer project — that is where Composer writes it, and a mid-sized Symfony
application vendors more PHP than it contains. A `composer.json` inside an
installed package is not a project root, so the graph does not depend on
whether `composer install` has run. `var/`, `cache/` and `.phpunit.cache/` are
excluded at any depth.

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
The alias map is per kind, as in PHP: an unqualified function call goes through
`use function` aliases only and an unqualified constant through `use const`
only. A class alias (`use Lib\Once;`) applies to class positions and to the
head of a qualified name (`Models\User`), so it does not capture `once()`, and
an unqualified call never binds to a class.
A name written before `::` (a static call, a static property, a class constant
or `::class`) is a class position, like a type, `new` or `instanceof`. It goes
through the class aliases, then the namespace, then PSR-4, and binds only to a
class, interface, trait or enum - never to a function or constant of the same
name (`Str::` does not reach `str()`). An undeclared qualifier is reported
under its namespaced name, because PHP does not fall back to the global
namespace for classes.
Receiver types are inferred for the cases that are syntactically recoverable and
overwhelmingly common — `$x = new Foo()` followed by `$x->m()`, promoted
constructor properties, `$this->`, and `self::` / `static::` / `parent::`.
The keywords themselves resolve too, in every position they can be written - a
type, `new`, `instanceof`, `::` and `::class`: `self` and `static` to the
enclosing class, `parent` to its first base class (a `HAS_TYPE` or `REFERENCES`
edge to it). Outside a class they resolve to nothing.

Trait methods resolve through the class that uses them. In the fixture,
`inspect(Base $item)` calling `$item->describe()` binds to
`Fixture\Models\Describes\describe` — the trait's method, which is where the
implementation actually is.

A member is looked up in PHP's order: the class itself, then its traits, then its base class (recursively, with the base's own traits). An interface's declaration is the target only when none of those declares the method.

Every `use` clause - plain, aliased, grouped, `use function` and `use const` - gets
a `RESOLVES_TO` with a position to the class, function or constant it names. The
`IMPORTS` edge to the namespace MODULE stays. A namespace alias resolves to its
MODULE, and a vendor name to an EXTERNAL_SYMBOL marked `external`.

Using a trait produces an `INHERITS_FROM` edge, the same as `extends` and
`implements`. PHP traits are mixed in rather than inherited, so this is a
modelling choice: the graph records that the methods arrive from there, which is
the question a call graph is asked.

## What it cannot answer

Stated plainly, because `degraded` is the honest status and not a formality:

- **`$var->method()` where `$var`'s type comes from anywhere else** — a function
  return, an array element, a parameter with no type declaration.
- **Dynamic class names** — `new $cls` and `$cls::method()` are followed only
  while `$cls` is assigned one `Foo::class` and nothing else.
- **Late static binding** — `static` is taken to be the class it is written in, so
  `static::m()` in a base class lands on the base's `m`, not on a subclass override.
- **`__get`** — nothing declares what it accepts. `__call` and `__callStatic` are
  followed: a method no class of the hierarchy declares is a call of the one that
  does, unless a base class or trait outside the project may declare it.
- **Callables.** A string, `'A::m'`, `[$o, 'm']` or `g(...)` is a value the flow
  pass follows to the parameter it is handed to, through locals and `$this->name`
  properties, and it is a call where one of those is called (`$f()`,
  `($this->f)()`, `call_user_func($f)`). The pass ignores order: a parameter
  reassigned before the call holds both values. Where it is written, `g(...)`,
  `A::m(...)` or `$o->m(...)` is also a `REFERENCES` edge (access `read`) from the
  enclosing scope to the function or method, and never a `CALLS` edge at that site.
- **Anything in `vendor/`** — deliberately not collected, so a call into a
  framework becomes an `EXTERNAL_SYMBOL`. That is the right answer for a call
  graph of *your* code, but it means callix cannot tell you what
  `$container->get('service')` returns.

Everything unresolved still produces an edge to an `EXTERNAL_SYMBOL`, so a
reference is never silently dropped. The exception is syntax: language
constructs that look like calls (`isset`, `empty`, `eval`, `die`/`exit`, ...)
and magic constants (`__DIR__`, `__CLASS__`, ...) are not symbols, so they
produce no edge, while the calls inside their arguments still do. What the
edge says about itself is its `resolution`: `definition` for a declaration of the project, `external` for a
name no analysed source declares (a built-in such as `explode`, a library), and
`unlocated` for a member of a class the project does declare that the table
could not find on it (inherited from `vendor/`, declared only on an
implementation, or magic). On laravel/framework, of 128 874 calls, 62 898 reach a
definition, 24 605 are outside the project and 41 371 are `unlocated`: that last
number is the one to read before trusting a PHP call graph.

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
