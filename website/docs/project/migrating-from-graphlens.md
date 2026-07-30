---
sidebar_position: 1
---

# Migrating from graphlens

callix is a rewrite of [graphlens](https://github.com/Neko1313/graphlens) by the
same author. The graph contract did not change, so most code moves across with
an import swap.

## What stayed the same

The same 14 node kinds and 12 relation kinds, the same deterministic ID formula,
the same 1-based spans, and the same serialization format. A graph written by
graphlens reads in callix and the other way round.

The structural half of the graph is byte-identical. That is not a claim, it is
the project's test harness: both implementations analyse the same repository,
both graphs are serialized with sorted keys, and the texts are diffed. Verified
on apache/superset, colinhacks/zod, gin-gonic/gin, BurntSushi/ripgrep and
astral-sh/ruff.

## What changed

**Packaging.** graphlens was a workspace of nine Python packages with one
adapter each. callix is one wheel with every adapter inside.

**Resolvers.** graphlens drove language servers over JSON-RPC and asked you to
install them: `ty`, `gopls`, `rust-analyzer`, `intelephense`. callix links ty
and typescript-go into the module, so Python and TypeScript need nothing. Go
and Rust still need their toolchain, but no separate language server — `gopls`
in particular is no longer required.

**Adapter construction.** There is no registry:

```python
# graphlens
adapter = adapter_registry.load("python")()

# callix
from callix import PythonAdapter
adapter = PythonAdapter()
```

**Writing an adapter.** Adapters are Rust modules now, not Python classes, so a
new language means a change to callix rather than a package of your own. The
resolver and dependency-parser hooks are still Python objects — see
[Custom resolvers](../guides/custom-resolvers.md).

## What is not here

`graphlens-cli`, the MCP server, the Neo4j backend, the HTML visualization,
`graphlens-link`, and the PHP and C# adapters. callix produces the graph; the
layers above it have not been ported.

## The one deliberate divergence

When a resolver finds a definition it cannot name, the synthetic
`EXTERNAL_SYMBOL` needs a key. graphlens used `{role}@{line}:{col}` — no file
path — so sites sharing coordinates across different files collapsed into one
node. On apache/superset that merged 98% of external nodes: 75,806 of 77,041.

callix includes the project-relative path:

```
graphlens:  read@399:60
callix:     read@superset/commands/importers/v1/utils.py:399:60
```

Relative on purpose — an absolute path would stop IDs from matching between
machines. The consequence is that these nodes have different IDs in the two
implementations, and there are more of them. Edges, structural nodes and the
resolved share are unchanged.

## Two bugs fixed along the way

**Go stdlib classification.** graphlens resolves symlinks on the target path
only and reads GOROOT verbatim. On Debian-like installs `/usr/lib/go-X/src` is
a symlink to `/usr/share/go-X/src`, so the entire standard library classified
as `unknown`. callix compares both the original and the resolved path on both
sides.

**Path ordering.** Python compares `pathlib.Path` component by component, not
as whole strings. Sorting by the string diverges on neighbours like
`src/v4-mini/…` and `src/v4/…`, which changed file order and therefore node
order. callix reproduces the component-wise comparison.
