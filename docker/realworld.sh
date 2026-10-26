#!/bin/sh
# Runs callix on real projects, which the fixtures cannot stand in for: scale,
# real module graphs, a resolver that has to load a real workspace.
#
#   sh docker/realworld.sh binary /path/to/callix
#   sh docker/realworld.sh image  IMAGE [full|slim]
#
# Per project it analyses (--strict where the resolver is supposed to come up
# fully resolved), exports LSIF and SCIP, and checks the LSIF against the
# sources with docker/check_lsif.py.
set -eu

MODE="${1:?usage: realworld.sh binary BIN | image IMAGE [full|slim]}"
TARGET="${2:?usage: realworld.sh binary BIN | image IMAGE [full|slim]}"
FLAVOUR="${3:-full}"
HERE="$(cd "$(dirname "$0")" && pwd)"

work="$(mktemp -d)"
chmod 777 "$work"

# Cargo writes `target/` into the mounted workspace as root: clean up through
# the image, and never let a cleanup problem change the verdict.
cleanup() {
    rm -rf "$work" 2>/dev/null && return 0
    if [ "$MODE" = image ]; then
        docker run --rm -v "$work:/w" --entrypoint sh "$TARGET" \
            -c 'rm -rf /w/* /w/.[!.]* 2>/dev/null; true' >/dev/null 2>&1 || true
    fi
    rm -rf "$work" 2>/dev/null || true
}
trap cleanup EXIT

# Where callix sees the work directory.
if [ "$MODE" = image ]; then
    inside=/workspace
    cx() { docker run --rm -v "$work:/workspace" "$TARGET" "$@"; }
else
    inside="$work"
    cx() { "$TARGET" "$@"; }
fi

# project <owner/repo> <name> <min-ranges> <analyze flags...>
project() {
    repo="$1"; name="$2"; min="$3"; shift 3
    echo "== $name ($repo)"
    git clone -q --depth 1 "https://github.com/$repo.git" "$work/$name"
    chmod -R a+rwX "$work/$name"

    cx analyze "$inside/$name" "$@" -o "$inside/$name.json"
    cx export "$inside/$name.json" -f lsif -q --source-root "$inside/$name" -o "$inside/$name.lsif"
    cx export "$inside/$name.json" -f scip -q --source-root "$inside/$name" -o "$inside/$name.scip"
    test -s "$work/$name.scip" || { echo "realworld: empty SCIP for $name" >&2; exit 1; }
    python3 -I "$HERE/check_lsif.py" "$work/$name.lsif" "$min" "file://$inside/$name" "$work/$name"
}

# In the binary and in every image: Go resolves through the linked-in bridge
# when a toolchain is there, the C family through its own symbol table.
if [ "$MODE" = binary ] || [ "$FLAVOUR" = full ]; then
    project gin-gonic/gin gin 10000 --prepare --strict -l go
    project google/leveldb leveldb 10000 -l cpp,c
    [ "$MODE" = binary ] && project psf/requests requests 5000 -l python
else
    # `slim` carries no Go or Rust toolchain; what it can do on its own is the
    # C family and Python (ty is linked in).
    project google/leveldb leveldb 10000 -l cpp,c
    project psf/requests requests 5000 -l python
fi

# Rust needs rust-analyzer and Cargo: the full image has them.
if [ "$MODE" = image ] && [ "$FLAVOUR" = full ]; then
    project BurntSushi/ripgrep ripgrep 10000 --prepare --strict -l rust
fi

echo "realworld: ok"
