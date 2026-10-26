#!/bin/sh
# Smoke test for a built image, run against the repository's own fixtures.
#
#   sh docker/smoke.sh IMAGE [full|slim]
#
# `full` additionally requires the Go and Rust resolvers to come up fully
# resolved; `slim` only checks what is linked into the binary.
set -eu

IMAGE="${1:?usage: smoke.sh IMAGE [full|slim]}"
FLAVOUR="${2:-full}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

work="$(mktemp -d)"

# Analysing a Rust project makes cargo write `target/` into the mounted
# workspace as root, which the invoking user cannot remove: clean up through the
# image, and never let a cleanup problem change the verdict.
cleanup() {
    rm -rf "$work" 2>/dev/null && return 0
    docker run --rm -v "$work:/w" --entrypoint sh "$IMAGE" \
        -c 'rm -rf /w/* /w/.[!.]* 2>/dev/null; true' >/dev/null 2>&1 || true
    rm -rf "$work" 2>/dev/null || true
}
trap cleanup EXIT
cp -R "$ROOT/tests/fixtures" "$work/fixtures"
chmod -R a+rwX "$work"

run() {
    docker run --rm -v "$work:/workspace" "$IMAGE" "$@"
}

fail() {
    echo "smoke: FAIL: $*" >&2
    exit 1
}

echo "== version"
run --version

echo "== doctor"
doctor="$(run doctor)"
echo "$doctor"
if [ "$FLAVOUR" = full ]; then
    for tool in go cargo rust-analyzer npm python3; do
        echo "$doctor" | grep -q "^  ok  *$tool " || fail "$tool is missing from the full image"
    done
fi

echo "== the binary links no libpython"
if docker run --rm --entrypoint sh "$IMAGE" -c 'ldd /usr/local/bin/callix' | grep -qi python; then
    fail "callix links libpython; the image is meant to need no Python for it"
fi

echo "== python fixture: LSIF"
run export fixtures/python -f lsif -q -o python.lsif
head -c 200 "$work/python.lsif" | grep -q '"label":"metaData"' || fail "LSIF does not start with metaData"
[ "$(wc -l < "$work/python.lsif")" -gt 100 ] || fail "LSIF is suspiciously short"

echo "== python fixture: strict passes"
run analyze fixtures/python --strict -q -o python.json

echo "== php fixture: strict refuses (exit 3)"
code=0
run analyze fixtures/php --strict -q -o php.json || code=$?
[ "$code" = 3 ] || fail "expected exit 3 from --strict on PHP, got $code"

echo "== every language in one run"
run export fixtures -f lsif -q -o all.lsif
[ -s "$work/all.lsif" ] || fail "polyglot export is empty"

if [ "$FLAVOUR" = full ]; then
    echo "== go fixture resolves"
    run analyze fixtures/go -l go --strict -q -o go.json
    echo "== rust fixture resolves"
    run analyze fixtures/rust -l rust --strict -q -o rust.json
fi

echo "smoke: ok ($IMAGE, $FLAVOUR)"
