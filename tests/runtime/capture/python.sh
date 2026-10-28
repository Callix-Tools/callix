#!/usr/bin/env bash
# Capture the runtime call graph of apache/superset 6.1.0 and write
# tests/runtime/apache__superset.json (see tests/runtime/README.md).
#
# Prerequisites: git, uv (https://docs.astral.sh/uv/; it fetches CPython 3.12 when
# python3.12 is not installed), network access to PyPI, ~1 GB in $WORK.
# CPython 3.12, not 3.13: superset pins numpy 1.26.4 and pandas 2.1.4, which have
# no 3.13 wheels; sys.monitoring needs 3.12+.
#
#   tests/runtime/capture/python.sh              # everything, ~15 min on 1 CPU
#   CHECKOUT=/opt/xref/src/apache__superset WORK=/tmp/rt tests/runtime/capture/python.sh
#
# CHECKOUT is the pristine checkout `runtime.py compare` analyses (default: the
# compare default workdir); it is cloned when missing and never written to. The
# project runs from a `git archive` export in $WORK/proj, so the venv, caches
# and pytest output never land in the analysed tree and lines stay identical.
# Workload: every test of tests/unit_tests (5605 collected); six fail here and
# that is expected (four MySQL tests need mysqlclient, two need prophet, both
# left out of the install because they need system headers or a Stan toolchain).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
PROJECT=apache/superset
REF=6.1.0
CHECKOUT="${CHECKOUT:-/tmp/callix-baseline/apache__superset}"
WORK="${WORK:-/tmp/callix-runtime/python}"
OUT="${OUT:-$REPO/tests/runtime/apache__superset.json}"
PY="${PYTHON:-3.12}"
EXCLUDED='^(-e |mysqlclient==|python-ldap==|prophet==|cmdstanpy==|stanio==)'
TESTS="${TESTS:-tests/unit_tests}"   # a shorter subset for a quick try
TRACE=(-m pytest -p no:cacheprovider -q $TESTS)
if [ "$TESTS" = "tests/unit_tests" ]; then
    WORKLOAD="superset tests/unit_tests (pytest, 5605 tests; 6 need mysqlclient or prophet and fail), CPython $PY, requirements/development.txt without mysqlclient, python-ldap, prophet"
else
    WORKLOAD="superset $TESTS (pytest subset, not the committed baseline), CPython $PY"
fi

if [ ! -d "$CHECKOUT/.git" ]; then
    git clone --quiet --depth 1 --branch "$REF" https://github.com/$PROJECT "$CHECKOUT"
fi
SHA="$(git -C "$CHECKOUT" rev-parse HEAD)"
[ "$SHA" = "$(git -C "$CHECKOUT" rev-parse "$REF^{commit}")" ] \
    || { echo "$CHECKOUT is not at $REF" >&2; exit 1; }

rm -rf "$WORK/proj" && mkdir -p "$WORK/proj"
git -C "$CHECKOUT" archive HEAD superset superset-core superset-extensions-cli tests \
    requirements setup.py pyproject.toml pytest.ini MANIFEST.in README.md LICENSE.txt \
    NOTICE scripts superset-frontend/package.json | tar -x -C "$WORK/proj"

if [ ! -x "$WORK/venv/bin/python" ]; then
    uv venv --quiet -p "$PY" "$WORK/venv"
    grep -Ev "$EXCLUDED" "$WORK/proj/requirements/development.txt" > "$WORK/requirements.txt"
    uv pip install --quiet -p "$WORK/venv/bin/python" -r "$WORK/requirements.txt"
fi
uv pip install --quiet -p "$WORK/venv/bin/python" --no-deps \
    -e "$WORK/proj" -e "$WORK/proj/superset-core" -e "$WORK/proj/superset-extensions-cli"

# pytest's exit status is 1 (the six failures above); the trace is still whole
(cd "$WORK/proj" && "$WORK/venv/bin/python" "$HERE/python.py" trace --root . \
    --raw "$WORK/raw.json" -- "${TRACE[@]}") || true

"$WORK/venv/bin/python" "$HERE/python.py" convert "$WORK/raw.json" \
    --root "$WORK/proj" --project "$PROJECT" --ref "$REF" --sha "$SHA" \
    --workload "$WORKLOAD" \
    --command "tests/runtime/capture/python.sh (python tests/runtime/capture/python.py trace --root . -- ${TRACE[*]})" \
    -o "$OUT"
