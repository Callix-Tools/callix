#!/usr/bin/env bash
# Capture the runtime call graph of laravel/framework (PHP) with Xdebug.
#
#   tests/runtime/capture/php.sh
#
# Runs PHPUnit on a fixed set of laravel/framework suites, ONE PHP PROCESS PER
# SUITE, under the Xdebug profiler (an exact, per-call trace), converts each
# profile to a small edge list with capture/xdebug.py, deletes the profile and
# finally writes tests/runtime/laravel__framework.json.
#
# Prerequisites
#   - PHP >= 8.3 CLI with mbstring, intl, openssl, tokenizer, dom/xml,
#     pdo_sqlite, curl, ctype and filter
#   - Xdebug >= 3.2 (apt install php8.3-xdebug); it need not be enabled, it is
#     loaded per run with -d zend_extension=xdebug
#   - composer 2, git, python3
#   - about 1 GB free in WORK (the largest raw profile is ~650 MB) and about
#     5 minutes
#
# Settings (environment)
#   SRC     pristine checkout at the pinned ref, never modified
#           (default ${WORKDIR:-/tmp/callix-baseline}/laravel__framework)
#   WORK    scratch directory, deleted at the start
#           (default ${TMPDIR:-/tmp}/callix-runtime-php)
#   OUT     artifact (default tests/runtime/laravel__framework.json)
#   SUITES  directories under tests/ (default below); do not add View, the
#           artifact would pass 2 MB
#   JOBS    parallel git fetches for the dependencies (default 2)
#
# The build copy in WORK/proj is a clone of SRC at the same commit, so lines
# are identical and nothing is built inside SRC. Dependencies are installed by
# capture/composer_git.py (a shallow git fetch per package) because dist
# zipballs are not reachable everywhere and composer's fallback to full clones
# fills the cache with gigabytes. Never run a plain `composer install` before
# composer_git.py has filled vendor/.
#
# The suites that need an S3 client, phpstan, rector or pint are not run: those
# packages are removed from the COPY's composer.json only.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
PROJECT="laravel/framework"
SRC="${SRC:-${WORKDIR:-/tmp/callix-baseline}/laravel__framework}"
WORK="${WORK:-${TMPDIR:-/tmp}/callix-runtime-php}"
OUT="${OUT:-$REPO/tests/runtime/laravel__framework.json}"
SUITES="${SUITES:-Support Container Pipeline Events Config Routing Validation}"
JOBS="${JOBS:-2}"

for tool in php composer git python3; do
    command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 1; }
done

REF="$(python3 - "$REPO/benchmarks/projects.json" "$PROJECT" <<'PY'
import json, sys
for entry in json.load(open(sys.argv[1]))["projects"]:
    if entry["name"] == sys.argv[2]:
        print(entry["ref"])
        break
PY
)"
[ -n "$REF" ] || { echo "$PROJECT is not in benchmarks/projects.json" >&2; exit 1; }
SHA="$(git -C "$SRC" rev-parse HEAD)"
WANT="$(git -C "$SRC" rev-parse "$REF^{commit}")"
[ "$SHA" = "$WANT" ] || { echo "$SRC is at $SHA, $REF is $WANT" >&2; exit 1; }
[ -z "$(git -C "$SRC" status --porcelain)" ] || { echo "$SRC is not clean" >&2; exit 1; }

XDEBUG="-d zend_extension=xdebug"
if php -r 'exit(extension_loaded("xdebug") ? 0 : 1);'; then XDEBUG=""; fi
TOOL="xdebug $(php $XDEBUG -r 'echo phpversion("xdebug");') profiler (PHP $(php -r 'echo PHP_VERSION;'))"

[ -n "$WORK" ] && [ "$WORK" != "/" ] || { echo "bad WORK" >&2; exit 1; }
rm -rf "$WORK"
mkdir -p "$WORK/prof" "$WORK/parts"
export COMPOSER_HOME="$WORK/composer-home" COMPOSER_CACHE_DIR="$WORK/composer-cache"
export COMPOSER_ALLOW_SUPERUSER=1 COMPOSER_NO_INTERACTION=1

echo "== build copy at $SHA ($REF)"
git clone -q "$SRC" "$WORK/proj"
git -C "$WORK/proj" checkout -q "$SHA"
cd "$WORK/proj"
composer remove --dev --no-update --no-plugins --no-scripts \
    aws/aws-sdk-php league/flysystem-aws-s3-v3 phpstan/phpstan rector/rector laravel/pint
COMPOSER_ROOT_VERSION="${REF#v}" composer update --no-install --no-plugins --no-scripts \
    --ignore-platform-req='ext-*'
python3 "$HERE/composer_git.py" "$WORK/proj" --jobs "$JOBS"
composer install --no-plugins --no-scripts --ignore-platform-req='ext-*'

COMMAND="php -d opcache.enable_cli=0 -d opcache.jit=0 -d zend_extension=xdebug -d xdebug.mode=profile -d xdebug.start_with_request=yes -d xdebug.output_dir=\$WORK/prof -d xdebug.profiler_output_name=cachegrind.out.<Suite> vendor/bin/phpunit --colors=never tests/<Suite>; for <Suite> in $SUITES"
WORKLOAD="phpunit $SUITES, one process per suite, laravel/framework's own tests"

parts=()
for suite in $SUITES; do
    echo "== profiling tests/$suite"
    profile="$WORK/prof/cachegrind.out.$suite"
    rc=0
    php -d opcache.enable_cli=0 -d opcache.jit=0 $XDEBUG \
        -d xdebug.mode=profile -d xdebug.start_with_request=yes \
        -d "xdebug.output_dir=$WORK/prof" \
        -d "xdebug.profiler_output_name=cachegrind.out.$suite" \
        vendor/bin/phpunit --colors=never "tests/$suite" \
        > "$WORK/phpunit-$suite.log" 2>&1 || rc=$?
    # 0 = pass, 1 = failures, 2 = errors: the trace is valid either way.
    case "$rc" in
        0|1|2) ;;
        *) tail -n 30 "$WORK/phpunit-$suite.log" >&2
           echo "phpunit exited with $rc on $suite" >&2; exit 1 ;;
    esac
    tail -n 3 "$WORK/phpunit-$suite.log"
    python3 "$HERE/xdebug.py" reduce "$profile" --root "$WORK/proj" -o "$WORK/parts/$suite.json"
    rm -f "$profile"
    parts+=("$WORK/parts/$suite.json")
done

echo "== building $OUT"
python3 "$HERE/xdebug.py" build "${parts[@]}" --root "$WORK/proj" --project "$PROJECT" \
    --ref "$REF" --tool "$TOOL" --workload "$WORKLOAD" --command "$COMMAND" -o "$OUT"
size="$(stat -c %s "$OUT")"
echo "$OUT: $size bytes"
if [ "$size" -gt 2097152 ]; then
    echo "warning: the artifact is over 2 MB, drop a suite from SUITES" >&2
fi
