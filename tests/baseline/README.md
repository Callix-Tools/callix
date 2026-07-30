# Project baselines

Fingerprints of the fourteen projects in
[`benchmarks/projects.json`](../../benchmarks/projects.json), one JSON per
project — every adapter covered by at least one real repository. See
[`../baseline.py`](../baseline.py) for what a fingerprint contains and why whole
graphs are not stored.

```bash
task baseline                      # compare against these files
task baseline:capture              # rewrite them (clones what is missing)
task baseline -- --project astral-sh/ruff
task baseline -- --lang cpp        # one language at a time
```

Checkouts default to `/tmp/callix-baseline`; point `--workdir` elsewhere on a
machine with a small `/tmp`, since ruff alone is 1.2 GB.

These are **not** part of `task test` or CI: they need ~3 GB of clones and
several minutes. Run them when you touch a visitor, the graph model, or path
handling — the places where a change is easy to make and hard to see.

Prefer `--lang` over a bare run while iterating. A full capture rewrites all
fourteen files, and the eight originals include the ones whose resolvers are the
slow part — ruff alone spends four minutes in `rust-analyzer scip`.

## What each language's entries are for

| Adapter | Projects | What the entry pins down |
|---|---|---|
| Python | apache/superset | the visitor, and ty across a release |
| TypeScript | colinhacks/zod | the visitor, and typescript-go |
| Go | gin · casdoor · hugo | package-directory MODULEs, `go/types` |
| Rust | ripgrep · axum · ruff | second-pass `impl` dispatch, the SCIP decoder |
| PHP | laravel/framework | the symbol table on 2 873 files, 41% internal |
| C | redis/redis | header/source unification across 779 files |
| C++ | leveldb · rocksdb | the same at two scales, plus `X::y` scope classification |
| YAML | openapi-directory · argo-cd | the OpenAPI extractor; discovery and flavours |

PHP and the C family resolve through a symbol table, so their fingerprints
record `degraded` and — more usefully — the internal/external split. The C
family's `unresolved` is always 0 by construction: every miss becomes an
`EXTERNAL_SYMBOL` rather than a dropped edge, so `resolved` equals `queries` and
says nothing. A symbol-table regression shows up as `internal` falling while
`external` rises, which is why both are stored.

YAML records `ok` with **zero** queries. That is not an omission: the adapter
emits no occurrences at all, and pinning the zero is what would catch it
starting to.

## Cross-check against graphlens, 2026-07-30

[graphlens](https://github.com/Neko1313/graphlens) is the Python implementation
callix replaces. It is archived, so this record was taken while its environment
could still run — it is not reproducible indefinitely.

It covers the **original eight** projects only — the four languages graphlens
had adapters for. PHP, C, C++ and YAML have no counterpart there and never will,
so for those four the golden fixtures and these fingerprints are the whole net;
there is no second implementation to agree with.

Structural graphs (`resolve=False`) of those eight, compared per file:

| Project | Language | Divergence |
|---|---|---|
| apache/superset | Python | `DEPENDENCY` + `DEPENDS_ON` only |
| colinhacks/zod | TypeScript | `DEPENDENCY` + `DEPENDS_ON` only |
| gin-gonic/gin | Go | + `ATTRIBUTE`, `PARAMETER`, `IMPORTS`, `DEPENDENCY` |
| casdoor/casdoor | Go | same |
| gohugoio/hugo | Go | same |
| BurntSushi/ripgrep | Rust | same |
| tokio-rs/axum | Rust | same |
| astral-sh/ruff | Rust | same |

**Every difference is an intentional addition, and there are no others.**

Since that snapshot, one more divergence is deliberate: graphlens stores an
**absolute** `file_path` on every node its Python and TypeScript visitors
create, while the FILE node beside it is relative. callix makes all of them
relative. A graph is supposed to mean the same thing on another machine, and
`nodes_in_file("pkg/models.py")` is supposed to return the declarations in that
file rather than only the file itself. Expect Python and TypeScript file hashes
to differ from graphlens for that reason.

For Python and TypeScript the file-level hashes are identical throughout — not
one file differs — so parity with graphlens holds exactly where it was
established. The only extra nodes are the DEPENDENCY nodes callix emits from
manifests that graphlens declared in its enum but never produced.

For Go and Rust the same DEPENDENCY nodes appear, plus the PARAMETER and
ATTRIBUTE nodes, the IMPORTS edges and the type annotations that graphlens
never emitted for those two languages. That gap is why the file hashes move:
callix now says more about the same code. The alignment is described in
[the migration notes](https://callix-tools.github.io/callix/docs/project/migrating-from-graphlens).

What this record is for: if a future change makes Python or TypeScript diverge
from graphlens beyond the DEPENDENCY nodes, that is a regression, and this
table is the evidence of what the state was before it.
