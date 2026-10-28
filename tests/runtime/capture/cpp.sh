#!/usr/bin/env bash
# Capture the runtime call graph of google/leveldb (C++) with valgrind callgrind.
#
#   tests/runtime/capture/cpp.sh [WORKDIR]
#
# Writes tests/runtime/google__leveldb.json. Rerunnable: an existing WORKDIR
# (default: $TMPDIR/callix-runtime-cpp) keeps its clone and build tree.
#
# Prerequisites: git, cmake >= 3.9, ninja, a C/C++ compiler (g++), valgrind
# (callgrind; 3.22 recorded), python3, network access to github.com (leveldb
# and its googletest/benchmark submodules), ~300 MB of disk. About 5 minutes
# to build with JOBS=2 and 4 to 7 minutes of tracing.
#
# What it does:
#   1. clones google/leveldb at the ref pinned in benchmarks/projects.json into
#      WORKDIR/proj (never the checkout `runtime.py compare` analyses), with the
#      googletest and benchmark submodules the CMake build needs;
#   2. builds it out of tree, -O0 -g -fno-inline (no inlining, or calls vanish),
#      LEVELDB_BUILD_TESTS=ON;
#   3. runs every test binary under callgrind, skipping the test cases that run
#      for over a second natively (SLOW below) and the three stress binaries;
#   4. converts the traces with capture/callgrind.py: only calls whose both
#      ends are files of leveldb (third_party/ is excluded) are kept.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../.." && pwd)"
WORK="${1:-${TMPDIR:-/tmp}/callix-runtime-cpp}"
JOBS="${JOBS:-2}"
PYTHON="${PYTHON:-python3}"
URL="https://github.com/google/leveldb"
REF="1.23"
SHA="99b3c03b3284f5886f9ef9a4ef703d57373e61be"
OUT="$REPO_ROOT/tests/runtime/google__leveldb.json"

# Test cases left out: over a second natively, i.e. minutes under callgrind,
# and repetitions of paths the rest already executes.
declare -A FILTER=(
  [db_test]="-DBTest.MultiThreaded:DBTest.Randomized:DBTest.SparseMerge:DBTest.GetEncountersEmptyLevel:DBTest.L0_CompactionBug_Issue44_*:DBTest.NonWritableFileSystem:DBTest.ApproximateSizes:DBTest.RecoverDuringMemtableCompaction:DBTest.RepeatedWritesToSameKey"
  [skiplist_test]="-SkipTest.Concurrent*"
  [table_test]="-Harness.Randomized"
  [fault_injection_test]="-FaultInjectionTest.FaultTestNoLogReuse"
)
# Whole binaries left out: long loops over Put/compaction that db_test covers.
SKIP=" autocompact_test issue178_test issue320_test "

mkdir -p "$WORK"
if [ ! -d "$WORK/proj/.git" ]; then
  git clone --quiet "$URL" "$WORK/proj"
fi
git -C "$WORK/proj" -c advice.detachedHead=false checkout --quiet "$SHA"
git -C "$WORK/proj" submodule update --init --depth 50 third_party/googletest third_party/benchmark

FLAGS="-O0 -g -fno-inline -Wno-error=maybe-uninitialized"
cmake -S "$WORK/proj" -B "$WORK/build" -G Ninja -DCMAKE_BUILD_TYPE=Debug \
  -DCMAKE_C_FLAGS="$FLAGS" -DCMAKE_CXX_FLAGS="$FLAGS" \
  -DLEVELDB_BUILD_TESTS=ON -DLEVELDB_BUILD_BENCHMARKS=OFF >/dev/null
ninja -C "$WORK/build" -j "$JOBS" >/dev/null

TRACE="$WORK/trace"
rm -rf "$TRACE" "$WORK/tmp"
mkdir -p "$TRACE" "$WORK/tmp"
export TEST_TMPDIR="$WORK/tmp"
tests=()
for bin in "$WORK"/build/*_test; do
  name="$(basename "$bin")"
  [[ "$SKIP" == *" $name "* ]] || tests+=("$name")
done
run_one() {
  local name="$1" filter="$2" args=()
  [ -n "$filter" ] && args=("--gtest_filter=$filter")
  valgrind --tool=callgrind -q --callgrind-out-file="$TRACE/callgrind.out.$name" \
    "$WORK/build/$name" "${args[@]}" >"$TRACE/$name.log" 2>&1 \
    || { echo "FAILED: $name (see $TRACE/$name.log)" >&2; touch "$TRACE/$name.failed"; return 0; }
  echo "  traced $name"
}
for name in "${tests[@]}"; do
  while [ "$(jobs -rp | wc -l)" -ge "$JOBS" ]; do wait -n || true; done
  run_one "$name" "${FILTER[$name]:-}" &
done
wait
if compgen -G "$TRACE/*.failed" >/dev/null; then exit 1; fi

"$PYTHON" "$HERE/callgrind.py" "$TRACE"/callgrind.out.* \
  --root "$WORK/proj" --project google/leveldb --ref "$REF" --language cpp \
  --tool "valgrind callgrind-$(valgrind --version | sed 's/^valgrind-//')" \
  --workload "leveldb 1.23 test binaries (gtest, -O0 -g -fno-inline) under callgrind, without autocompact/issue178/issue320 and the test cases over 1s natively (see capture/cpp.sh)" \
  --command "tests/runtime/capture/cpp.sh" \
  -o "$OUT"
