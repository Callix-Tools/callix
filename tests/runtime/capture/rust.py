#!/usr/bin/env python3
"""
Capture the runtime call graph of BurntSushi/ripgrep (Rust) with callgrind.

    python3 tests/runtime/capture/rust.py --checkout /tmp/callix-baseline/BurntSushi__ripgrep

Prerequisites: valgrind (3.22 or newer, for callgrind and DWARF 5), a Rust
toolchain with cargo (the build is `--locked`, so the crates of Cargo.lock must
be fetchable or already in ~/.cargo), gzip, git and python3 >= 3.10. The
checkout must be clean and at the pinned ref (`benchmarks/projects.json`).

What it does, in a scratch directory (`--work`, default a temporary one):

1. exports the checkout's HEAD with `git archive` (no build output ever lands
   in the analysed checkout, and every line stays where it is);
2. rewrites `#[inline(always)]` to `#[inline]` in place (same line count):
   LLVM's always-inliner runs even at opt-level 0, and an inlined function's
   callees would be charged to its caller;
3. builds `rg` and the library unit tests of the crates in TEST_CRATES with the
   dev profile at opt-level 0 and full debug info;
4. runs WORKLOAD (rg searches over its own `crates/` tree) and the unit tests
   under `valgrind --tool=callgrind`, one output file per process;
5. converts with `callgrind.py`, keeps only functions that ripgrep's own
   source defines (see `keep_function`) and writes the artifact.
"""

from __future__ import annotations

import argparse
import gzip
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent.parent
sys.path.insert(0, str(HERE))

import callgrind  # noqa: E402
import common  # noqa: E402

PROJECT = "BurntSushi/ripgrep"
LANGUAGE = "rust"
TEST_CRATES = ("globset", "grep-matcher", "grep-printer", "grep-regex", "grep-searcher")

# Every search runs single-threaded (-j1) except the last, which takes the
# parallel walker. `{data}` is the scratch data directory.
WORKLOAD = (
    ["-j1", r"fn \w+_search", "crates"],
    ["-j1", "-i", "-n", "SEARCHER", "crates"],
    ["-j1", "-w", "-c", "Matcher", "crates"],
    ["-j1", "-F", "-l", "impl<", "crates"],
    ["-j1", "-v", "-c", "e", "crates/matcher"],
    ["-j1", "--json", "-C1", "LineTerminator", "crates/searcher"],
    ["-j1", "-t", "rust", "--count-matches", "unsafe", "crates"],
    ["-j1", "-T", "rust", "--files-with-matches", "ripgrep", "crates"],
    ["-j1", "--files", "--sort", "path", "crates"],
    ["-j1", "-g", "*.toml", "--files", "crates"],
    ["-j1", "-U", r"fn new\(\)\s*->\s*\w+", "crates"],
    ["-j1", "-o", "--column", "--vimgrep", r"\bSink\w*", "crates/searcher"],
    ["-j1", "-r", "$1-X", "-e", r"(\w+)Builder", "crates/globset"],
    ["-j1", "--color", "always", "--heading", "-m2", "-A2", "-B1", "regex", "crates/regex"],
    ["-j1", "--passthru", "--stats", "Searcher", "crates/searcher/src/sink.rs"],
    ["-j1", "-e", "TODO", "-e", "FIXME", "--no-ignore", "--hidden", "crates"],
    ["-j1", "-z", "needle", "{data}/small.txt.gz"],
    ["-j1", "needle", "{data}/binary.dat"],
    ["-j1", "-a", "needle", "{data}/binary.dat"],
    ["-j1", "--binary", "needle", "{data}"],
    ["-j1", "-E", "utf-16le", "needle", "{data}/utf16.txt"],
    ["-j1", "--null-data", "needle", "{data}/binary.dat"],
    ["--type-list"],
    ["-j2", "--sort-files", "-c", "fn", "crates"],
)

DERIVABLE = re.compile(
    r"^(?:core|std)::(?:clone::Clone|marker::Copy|default::Default|fmt::Debug"
    r"|cmp::(?:PartialEq|Eq|PartialOrd|Ord)|hash::Hash)$"
)
_FN_LINE = re.compile(r"\bfn\b|\|")


def _run(cmd, **kwargs):
    print("+ " + " ".join(str(c) for c in cmd), file=sys.stderr, flush=True)
    return subprocess.run([str(c) for c in cmd], check=True, **kwargs)


def _split_qualified(name):
    """`<A as B>::rest` -> (A, B, rest), `<A>::rest` -> (A, None, rest), else None."""
    if not name.startswith("<"):
        return None
    depth = 0
    for index, char in enumerate(name):
        if char == "<":
            depth += 1
        elif char == ">":
            depth -= 1
            if depth == 0:
                inner, rest = name[1:index], name[index + 1:]
                break
    else:
        return None
    depth = 0
    for position, char in enumerate(inner):
        if char in "<([":
            depth += 1
        elif char in ">)]":
            depth -= 1
        elif depth == 0 and inner.startswith(" as ", position):
            return inner[:position], inner[position + 4:], rest
    return inner, None, rest


def _crate_of(path):
    """The crate a path names: `&mut dyn grep_matcher::Matcher` -> grep_matcher."""
    path = re.sub(r"^(?:&\s*(?:'\w+\s+)?(?:mut\s+)?|\*(?:const|mut)\s+|dyn\s+)+", "", path.strip())
    if path.startswith("<"):
        parts = _split_qualified(path)
        return _crate_of(parts[0]) if parts else None
    head = re.split(r"::|<", path, maxsplit=1)[0]
    return head or None


def owner_crates(name):
    """The crates whose code may define the function `name` (self type and trait)."""
    parts = _split_qualified(name)
    if parts is None:
        return {_crate_of(name)} - {None}
    holder, trait, _rest = parts
    return {_crate_of(holder), _crate_of(trait or "")} - {None}


def derived_trait(name):
    """The std trait of a `<T as Trait>::m` whose trait is derivable, else None."""
    parts = _split_qualified(name)
    if parts is None or parts[1] is None:
        return None
    trait = re.sub(r"<.*$", "", parts[1].strip())
    return trait if DERIVABLE.match(trait) else None


def keep_function(file, name, line, crates, source_line):
    """
    True when the function is written in the project's source.

    `crates` are the project's crate names. Dropped: generic std code that the
    debug info places in a project file (`alloc::boxed::box_new_uninit`,
    `<NonNull<_>>::new_unchecked::precondition_check`, `<RangeFrom<usize> as
    SliceIndex<[u8]>>::index`), whose owner is no project crate; and the impls
    `#[derive(..)]` generates, which are placed on the attribute or the item
    and have no `fn` in the source. `source_line` is the text at `line`.
    """
    if not owner_crates(name) & crates:
        return False
    if derived_trait(name) and not _FN_LINE.search(source_line or ""):
        return False
    return True


def project_crates(src):
    """Crate names of the workspace's own targets (libs, bins, tests, examples)."""
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--offline"],
        cwd=src, capture_output=True, text=True, check=True,
    ).stdout
    names = set()
    for package in json.loads(out)["packages"]:
        for target in package["targets"]:
            names.add(target["name"].replace("-", "_"))
    return names


def filter_calls(calls, root, crates):
    """Drop every call with an end `keep_function` rejects."""
    lines_cache = {}

    def source_line(file, line):
        if file not in lines_cache:
            try:
                lines_cache[file] = (Path(root) / file).read_text(
                    encoding="utf-8", errors="replace").splitlines()
            except OSError:
                lines_cache[file] = []
        text = lines_cache[file]
        return text[line - 1] if 0 < line <= len(text) else ""

    verdict = {}

    def keep(end):
        if end not in verdict:
            file, name, line = end
            verdict[end] = keep_function(file, name, line, crates, source_line(file, line))
        return verdict[end]

    return [c for c in calls if keep(c[0]) and keep(c[1])]


def prepare(checkout, work, jobs):
    src = work / "src"
    if src.exists():
        shutil.rmtree(src)
    src.mkdir(parents=True)
    archive = subprocess.run(
        ["git", "-C", str(checkout), "archive", "--format=tar", "HEAD"],
        capture_output=True, check=True,
    ).stdout
    subprocess.run(["tar", "-x", "-C", str(src)], input=archive, check=True)
    for path in src.rglob("*.rs"):
        text = path.read_text(encoding="utf-8")
        if "#[inline(always)]" in text:
            path.write_text(text.replace("#[inline(always)]", "#[inline]"), encoding="utf-8")
    env = {
        **os.environ,
        "CARGO_PROFILE_DEV_OPT_LEVEL": "0",
        "CARGO_PROFILE_DEV_DEBUG": "2",
        "CARGO_INCREMENTAL": "0",
    }
    _run(["cargo", "build", "--locked", "--jobs", jobs, "--bin", "rg"], cwd=src, env=env)
    cmd = ["cargo", "test", "--locked", "--jobs", jobs, "--no-run", "--lib",
           "--message-format=json-render-diagnostics"]
    for crate in TEST_CRATES:
        cmd += ["-p", crate]
    print("+ " + " ".join(cmd), file=sys.stderr, flush=True)
    out = subprocess.run(cmd, cwd=src, env=env, stdout=subprocess.PIPE, text=True, check=True).stdout
    tests = []
    for line in out.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            tests.append(message["executable"])
    data = work / "data"
    data.mkdir(exist_ok=True)
    text = b"hay\nhay with a needle in it\nmore hay\n"
    with gzip.GzipFile(data / "small.txt.gz", "wb", mtime=0) as handle:
        handle.write(text)
    (data / "binary.dat").write_bytes(text + b"\x00\x01\x02 needle\x00\n" + text)
    (data / "utf16.txt").write_bytes(text.decode().encode("utf-16-le"))
    return src, sorted(tests), data


def run_workload(src, tests, data, traces):
    if traces.exists():
        shutil.rmtree(traces)
    traces.mkdir(parents=True)
    rg = src / "target" / "debug" / "rg"
    valgrind = ["valgrind", "--tool=callgrind", "--quiet",
                f"--callgrind-out-file={traces}/callgrind.out.%p"]
    for args in WORKLOAD:
        args = [a.replace("{data}", str(data)) for a in args]
        subprocess.run(
            valgrind + [rg, "--no-config", *args], cwd=src,
            stdout=subprocess.DEVNULL, check=False,
        )
    for test in tests:
        _run(valgrind + [test, "--test-threads=1", "-q"], cwd=src,
             stdout=subprocess.DEVNULL)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    parser.add_argument("--checkout", default="/tmp/callix-baseline/BurntSushi__ripgrep",
                        help="clean checkout at the pinned ref (runtime.py's default workdir)")
    parser.add_argument("--ref", default="14.1.1")
    parser.add_argument("--work", help="scratch directory (default: a temporary one)")
    parser.add_argument("--jobs", default="2")
    parser.add_argument("--keep", action="store_true", help="keep the scratch directory")
    parser.add_argument("--convert-only", action="store_true",
                        help="reuse the traces already in --work")
    parser.add_argument("-o", "--output", default=str(HERE.parent / "BurntSushi__ripgrep.json"))
    args = parser.parse_args(argv)

    checkout = Path(args.checkout).resolve()
    if subprocess.run(["git", "-C", str(checkout), "status", "--porcelain"],
                      capture_output=True, text=True, check=True).stdout.strip():
        raise SystemExit(f"{checkout} is not clean")
    work = Path(args.work or tempfile.mkdtemp(prefix="callix-rt-rust-")).resolve()
    traces = work / "traces"
    src = work / "src"
    if not args.convert_only:
        src, tests, data = prepare(checkout, work, args.jobs)
        run_workload(src, tests, data, traces)
    calls = callgrind.parse(sorted(traces.glob("callgrind.out.*")), str(src))
    calls = filter_calls(calls, src, project_crates(src))
    tool = subprocess.run(["valgrind", "--version"], capture_output=True, text=True,
                          check=True).stdout.strip().replace("valgrind-", "valgrind callgrind-")
    rustc = subprocess.run(["rustc", "--version"], capture_output=True, text=True,
                           check=True).stdout.split()[1]
    workload = (
        f"rg (dev profile, opt-level 0, #[inline(always)] -> #[inline], rustc {rustc}): "
        f"{len(WORKLOAD)} searches over its own crates/ tree (plain, -i, -w, -F, -v, --json, "
        "-t/-T, --files, -g, -U multiline, -o/--vimgrep, -r, --color/--heading/context, "
        "--passthru/--stats, --no-ignore/--hidden, -z on a .gz, binary detection, -a, "
        "--binary, -E utf-16le, --null-data, --type-list, -j2), then the lib unit tests "
        f"of {', '.join(TEST_CRATES)} with --test-threads=1"
    )
    artifact = common.build_artifact(
        project=PROJECT, ref=args.ref, sha=common.git_sha(checkout), language=LANGUAGE,
        tool=tool, workload=workload,
        command="python3 tests/runtime/capture/rust.py --checkout /tmp/callix-baseline/BurntSushi__ripgrep",
        calls=calls,
    )
    common.write_artifact(args.output, artifact)
    print(f"{args.output}: {len(artifact['functions'])} functions, {len(artifact['edges'])} edges")
    if not args.keep and not args.work:
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
