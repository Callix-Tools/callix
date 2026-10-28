#!/usr/bin/env bash
# Capture the runtime call graph of colinhacks/zod (TypeScript) while its vitest
# suite runs, and write tests/runtime/colinhacks__zod.json.
#
#   tests/runtime/capture/typescript.sh [CHECKOUT] [WORKDIR]
#
#   CHECKOUT  clean clone of colinhacks/zod at the pinned ref (default
#             /tmp/callix-baseline/colinhacks__zod, what `task runtime` clones).
#             It is only read: the build runs in a copy, so node_modules and
#             vitest caches never land in the checkout runtime.py analyses.
#   WORKDIR   scratch space (default /tmp/callix-runtime/typescript; ~0.5 GB).
#
# Prerequisites: git, python3, Node.js 22 with npm, pnpm 10.12.1 (zod's
# packageManager; `corepack enable` provides it), network access to the npm
# registry. No callix build is needed.
#
# How: tests/runtime/capture/jsprobe/ is a vite plugin that rewrites every
# function body under packages/zod/src (sources and tests) into
#   const __c = __rt.e(ID); try { BODY } finally { __rt.x(__c) }
# and keeps a shadow stack, so each call between two project functions is
# recorded exactly once per execution (instrumentation, not sampling). Nothing
# is inlined or bundled: vitest runs the TypeScript sources through
# esbuild's type stripping only. ~2 minutes on 2 workers.
set -euo pipefail

PROJECT=colinhacks/zod
REF=v4.4.3
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
CHECKOUT="$(realpath "${1:-/tmp/callix-baseline/colinhacks__zod}")"
WORK="${2:-/tmp/callix-runtime/typescript}"
OUT="$REPO/tests/runtime/colinhacks__zod.json"

SHA="$(git -C "$CHECKOUT" rev-parse HEAD)"
WANT="$(git -C "$CHECKOUT" rev-parse "$REF^{commit}")"
if [ "$SHA" != "$WANT" ]; then
  echo "error: $CHECKOUT is at $SHA, not $REF ($WANT)" >&2
  exit 1
fi
if [ -n "$(git -C "$CHECKOUT" status --porcelain)" ]; then
  echo "error: $CHECKOUT has local changes" >&2
  exit 1
fi

PROJ="$WORK/proj"
TRACE="$WORK/trace"
rm -rf "$PROJ" "$TRACE"
mkdir -p "$PROJ"
git -C "$CHECKOUT" archive "$SHA" | tar -x -C "$PROJ"

# zod's own toolchain (vitest, vite, esbuild) at the lockfile's versions; only
# the root and packages/zod, not the docs site.
(cd "$PROJ" && pnpm install --frozen-lockfile --ignore-scripts --filter ./ --filter ./packages/zod)

mkdir -p "$PROJ/.callix"
cp "$HERE"/jsprobe/*.mjs "$HERE"/jsprobe/package.json "$PROJ/.callix/"
(cd "$PROJ/.callix" && npm install --no-audit --no-fund --silent)

RUN="vitest run --config .callix/vitest.config.mjs"
(
  cd "$PROJ"
  CALLIX_PROJ="$PROJ" CALLIX_PACKAGE=packages/zod CALLIX_INSTRUMENT=packages/zod/src \
  CALLIX_SETUP=scripts/fail-on-console.ts CALLIX_TRACE_DIR="$TRACE" \
    pnpm exec $RUN
)

python3 "$HERE/jsprobe.py" "$TRACE" --root "$PROJ" \
  --project "$PROJECT" --ref "$REF" --sha "$SHA" --language typescript \
  --tool "callix jsprobe: @babel/parser 7.28.5 source instrumentation, vitest $(cd "$PROJ" && pnpm exec vitest --version | awk '{print $1}' | sed 's#.*/##'), node $(node --version)" \
  --workload "zod package vitest suite (packages/zod/src: v3, v4 classic, mini, core, locales tests; typecheck off), every function under packages/zod/src instrumented" \
  --command "tests/runtime/capture/typescript.sh <checkout of colinhacks/zod at $REF>" \
  -o "$OUT"
