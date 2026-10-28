#!/usr/bin/env bash
# Capture the runtime call graph of redis/redis (C) with valgrind callgrind.
#
#   tests/runtime/capture/c.sh [checkout]
#
# Prerequisites: git, make, a C compiler, python3, valgrind (callgrind), and
# about 300 MB of scratch space. Nothing is installed system wide.
#
# What it does:
#   1. clones redis/redis at the ref pinned in benchmarks/projects.json into
#      $WORK/redis (or uses the checkout given as $1, which must be at that ref
#      and is built in place, so pass a COPY, never the analysed checkout);
#   2. builds it without inlining (-O0 -fno-inline, libc malloc, no TLS);
#   3. runs redis-server under callgrind, drives it with the fixed command mix
#      in c_workload.redis (redis-cli) plus a short redis-benchmark, and shuts
#      it down;
#   4. converts the callgrind output with callgrind.py into
#      tests/runtime/redis__redis.json.
#
# Environment: WORK (default /tmp/callix-runtime-c), PORT (default 21379),
# JOBS (default 2), OUT (default tests/runtime/redis__redis.json). The
# workload is single-process on purpose: a forked child (BGSAVE,
# BGREWRITEAOF) writes its own callgrind file that repeats every count the
# parent had before the fork.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
PROJECT=redis/redis
REF="$(python3 -c 'import json,sys; print(next(p["ref"] for p in json.load(open(sys.argv[1]))["projects"] if p["name"]==sys.argv[2]))' "$ROOT/benchmarks/projects.json" "$PROJECT")"
WORK="${WORK:-/tmp/callix-runtime-c}"
PORT="${PORT:-21379}"
JOBS="${JOBS:-2}"
SRC="${1:-$WORK/redis}"
RUN="$WORK/run"
OUT="${OUT:-$ROOT/tests/runtime/redis__redis.json}"

mkdir -p "$WORK"
if [ ! -d "$SRC/.git" ]; then
    git clone --quiet --depth 1 --branch "$REF" https://github.com/redis/redis "$SRC"
fi
want="$(git -C "$SRC" rev-parse "$REF^{commit}")"
[ "$(git -C "$SRC" rev-parse HEAD)" = "$want" ] || { echo "$SRC is not at $REF" >&2; exit 1; }

BUILD=(make -C "$SRC" -j"$JOBS" MALLOC=libc BUILD_TLS=no OPTIMIZATION="-O0 -fno-inline")
"${BUILD[@]}" >"$WORK/build.log" 2>&1 || { tail -30 "$WORK/build.log" >&2; exit 1; }
[ -z "$(git -C "$SRC" status --porcelain --untracked-files=no)" ] \
    || { echo "the build changed tracked files; lines would shift" >&2; exit 1; }

rm -rf "$RUN"; mkdir -p "$RUN"
BIN="$SRC/src"
SERVER=(valgrind --tool=callgrind --dump-instr=no --collect-jumps=no
        "--callgrind-out-file=$RUN/callgrind.out.%p"
        "$BIN/redis-server" --port "$PORT" --bind 127.0.0.1 --save "" --appendonly yes
        --dir "$RUN" --enable-debug-command yes --daemonize no --logfile "$RUN/redis.log")
BENCH=("$BIN/redis-benchmark" -p "$PORT" -n 500 -c 4 -r 1000 -q
       -t set,get,incr,lpush,rpush,lpop,rpop,sadd,hset,spop,zadd,zpopmin,lrange_100,mset,xadd)

"${SERVER[@]}" >"$RUN/valgrind.log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT
up=0
for _ in $(seq 120); do
    if "$BIN/redis-cli" -p "$PORT" PING 2>/dev/null | grep -q PONG; then up=1; break; fi
    sleep 1
done
[ "$up" = 1 ] || { echo "redis-server never answered PING on port $PORT" >&2; exit 1; }
"$BIN/redis-cli" -p "$PORT" <"$HERE/c_workload.redis" >"$RUN/cli.out"
"${BENCH[@]}" >"$RUN/bench.out"
"$BIN/redis-cli" -p "$PORT" SHUTDOWN NOSAVE >/dev/null 2>&1 || true
wait "$pid" || true
trap - EXIT

files=("$RUN"/callgrind.out.*)
[ "${#files[@]}" -eq 1 ] || { echo "expected one callgrind file, got ${#files[@]}" >&2; exit 1; }

# The stored command names no machine paths: <checkout> is the redis build tree,
# <run> the scratch run directory. printf %q keeps the quoting of every word.
q() { printf '%q ' "$@"; }
command="tests/runtime/capture/c.sh: $(q "${BUILD[@]}")&& $(q "${SERVER[@]}")& $(q "$BIN/redis-cli" -p "$PORT") < tests/runtime/capture/c_workload.redis; $(q "${BENCH[@]}"); $(q "$BIN/redis-cli" -p "$PORT" SHUTDOWN NOSAVE)"
command="${command//$BIN/<checkout>/src}"
command="${command//$SRC/<checkout>}"
command="${command//$RUN/<run>}"

tool="valgrind $(sed -n 's/^creator: //p' "${files[0]}" | head -1)"

python3 "$HERE/callgrind.py" "${files[@]}" --root "$SRC" --project "$PROJECT" --ref "$REF" \
    --language c --tool "$tool" \
    --workload "redis-server under callgrind: tests/runtime/capture/c_workload.redis via redis-cli (strings, bitmaps, HLL, lists, hashes, sets, sorted sets, geo, streams, expiry, MULTI/EXEC/WATCH, Lua EVAL, FUNCTION/FCALL, ACL, CONFIG, vector sets, SAVE, DEBUG RELOAD, AOF on), then redis-benchmark -n 500 -c 4 -r 1000 over 15 commands" \
    --command "$command" \
    -o "$OUT"
