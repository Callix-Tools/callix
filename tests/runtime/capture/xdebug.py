#!/usr/bin/env python3
"""Turn Xdebug profiler output (PHP) into a runtime artifact.

Xdebug writes callgrind text with ONE block per call instance, in post-order:
a block lists its `calls=` and the blocks of those callees were written just
before it. That makes the call tree recoverable, which the per-function
aggregation of `callgrind.py` cannot do, and PHP builtins (`fl=php:internal`)
need it: `array_map($f, $xs)` is a builtin between the caller and `$f`.

    xdebug.py reduce PROFILE... --root ROOT -o PART.json
    xdebug.py build PART.json... --root ROOT --project owner/repo --ref REF \
        --tool TOOL --workload TEXT --command TEXT -o ARTIFACT.json

`reduce` keeps a small file of relative edges per profile, so a multi-gigabyte
profile can be deleted before the next suite runs; `build` merges the parts.

Edges: a project function calling a project function is an edge. A project
function that calls builtins which end in a project function is an edge only
when EVERY builtin on the chain takes a callable (BRIDGE below): the caller of
`array_map` is the caller of its callback, as in the accuracy suites. Other
builtins (count, json_encode, iterator_to_array, constructors made by
Reflection, autoload) stop the chain.
"""

from __future__ import annotations

import argparse
import collections
import functools
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import common  # noqa: E402

INTERNAL = "php:internal"
BRIDGE = frozenset({
    "call_user_func", "call_user_func_array",
    "forward_static_call", "forward_static_call_array",
    "array_map", "array_filter", "array_walk", "array_walk_recursive",
    "array_reduce", "array_find", "array_find_key", "array_any", "array_all",
    "usort", "uasort", "uksort",
    "preg_replace_callback", "preg_replace_callback_array", "iterator_apply",
    "Closure->__invoke", "Closure->call",
    "ReflectionMethod->invoke", "ReflectionMethod->invokeArgs",
    "ReflectionFunction->invoke", "ReflectionFunction->invokeArgs",
})
CLOSURE = re.compile(r"\{(closure|anonymous-class):[^{}]*?:(\d+)(?:-(\d+))?\}")
PSEUDO = re.compile(r"^(?:include|include_once|require|require_once)::")
CALLBACK_SUFFIX = re.compile(r":\{.*\}$")
MAGIC_DECL = re.compile(r"function\s+&?\s*(__call|__callStatic)\s*\(", re.IGNORECASE)


def blocks(path):
    """Yield one dict per profile block: fl, fn, line (first cost line), calls."""
    files: dict[str, str] = {}
    funcs: dict[str, str] = {}

    def resolve(value: str, table: dict[str, str]) -> str:
        value = value.strip()
        if value.startswith("("):
            end = value.index(")")
            ident, name = value[1:end], value[end + 1:].strip()
            if name:
                table[ident] = name
                return name
            return table[ident]
        return value

    cur = None
    cfl = cfn = None
    expect_cost = False
    with open(path, encoding="utf-8", errors="replace") as handle:
        for raw in handle:
            line = raw.rstrip("\n")
            if expect_cost:
                expect_cost = False
                cur["calls"].append((cfl, cfn))
                continue
            if line.startswith("fl="):
                if cur is not None and cur["fn"]:
                    yield cur
                cur = {"fl": resolve(line[3:], files), "fn": None, "line": 0, "calls": []}
            elif cur is None:
                continue
            elif line.startswith("fn="):
                cur["fn"] = resolve(line[3:], funcs)
            elif line.startswith("cfl="):
                cfl = resolve(line[4:], files)
            elif line.startswith("cfn="):
                cfn = resolve(line[4:], funcs)
            elif line.startswith("calls="):
                expect_cost = True
            elif line[:1].isdigit() and cur["fn"] and cur["line"] == 0 and not cur["calls"]:
                cur["line"] = int(line.split()[0])
    if cur is not None and cur["fn"]:
        yield cur


def is_bridge(name: str) -> bool:
    name = name[len("php::"):] if name.startswith("php::") else name
    return CALLBACK_SUFFIX.sub("", name) in BRIDGE


def edges(path):
    """
    Return (counts, lines) of one profile.

    counts: {((file, fn), (file, fn)): calls} with absolute files, user to
    user (direct, or through callable-taking builtins only); lines: {(file,
    fn): smallest first cost line}. Raises ValueError when the post-order tree
    does not match the `calls=` lines (an Xdebug that aggregates).
    """
    counts: collections.Counter = collections.Counter()
    lines: dict = {}
    stack: list = []
    for block in blocks(path):
        k = len(block["calls"])
        if k > len(stack):
            raise ValueError(f"{path}: {block['fn']} lists {k} calls, {len(stack)} seen")
        children = stack[len(stack) - k:] if k else []
        for (_, cfn), child in zip(block["calls"], children):
            if child[0][1] != cfn:
                raise ValueError(
                    f"{path}: call tree mismatch under {block['fn']}: "
                    f"expected {cfn}, found {child[0][1]}"
                )
        if k:
            del stack[len(stack) - k:]
        internal = block["fl"] == INTERNAL
        key = (block["fl"], block["fn"])
        reach: list = []
        for child_key, child_internal, child_reach in children:
            targets = child_reach if child_internal else [child_key]
            if internal:
                if is_bridge(block["fn"]):
                    reach.extend(targets)
            else:
                for target in targets:
                    counts[(key, target)] += 1
        if not internal:
            old = lines.get(key, 0)
            if block["line"] and (old == 0 or block["line"] < old):
                lines[key] = block["line"]
            else:
                lines.setdefault(key, old)
        stack.append((key, internal, reach))
    return counts, lines


def normalize(name: str) -> str:
    """Drop the absolute path of closures and anonymous classes."""
    return CLOSURE.sub(
        lambda m: "{%s:%s%s}" % (m.group(1), m.group(2), "-" + m.group(3) if m.group(3) else ""),
        name,
    )


@functools.lru_cache(maxsize=None)
def _source(path: str) -> tuple:
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            return tuple(handle.read().split("\n"))
    except OSError:
        return ()


def trampoline(abs_file: str, name: str, line: int) -> str:
    """
    Xdebug names a `__call`/`__callStatic` frame by the called name
    (`Collection->adults`). When the source line holds the magic method's
    declaration, return the name of the magic method instead.
    """
    source = _source(abs_file)
    if not line or line > len(source):
        return name
    match = MAGIC_DECL.search(source[line - 1])
    if match is None:
        return name
    magic = match.group(1)
    separator = "::" if magic.lower() == "__callstatic" else "->"
    return re.split(r"->|::", name)[0] + separator + magic


def _skip(key) -> bool:
    file, name = key
    return name == "{main}" or bool(PSEUDO.match(name)) or "eval()'d code" in file


def reduce(paths, root):
    """Relative `[[file, name, line], [file, name, line], count]` triples."""
    counts: collections.Counter = collections.Counter()
    lines: dict = {}
    for path in paths:
        part_counts, part_lines = edges(path)
        counts.update(part_counts)
        for key, line in part_lines.items():
            old = lines.get(key, 0)
            if line and (old == 0 or line < old):
                lines[key] = line
            else:
                lines.setdefault(key, old)
    relative: dict = {}

    def rel(file: str):
        if file not in relative:
            relative[file] = common.relativize(file, root)
        return relative[file]

    merged: collections.Counter = collections.Counter()
    for (caller, callee), count in counts.items():
        ends = []
        for key in (caller, callee):
            if _skip(key) or rel(key[0]) is None:
                break
            line = lines.get(key, 0)
            ends.append((rel(key[0]), normalize(trampoline(key[0], key[1], line)), line))
        else:
            if caller != callee and ends[0] == ends[1]:
                continue  # a trampoline frame and its __call frame renamed alike
            merged[(ends[0], ends[1])] += count
    return [[list(a), list(b), n] for (a, b), n in sorted(merged.items())]


def cmd_reduce(args) -> int:
    triples = reduce(args.profiles, args.root)
    Path(args.output).write_text(json.dumps(triples), encoding="utf-8")
    print(f"{args.output}: {len(triples)} edges")
    return 0


def cmd_build(args) -> int:
    calls = []
    for part in args.parts:
        for caller, callee, count in json.loads(Path(part).read_text(encoding="utf-8")):
            calls.append((tuple(caller), tuple(callee), count))
    artifact = common.build_artifact(
        project=args.project, ref=args.ref, sha=common.git_sha(args.root),
        language="php", tool=args.tool, workload=args.workload,
        command=args.command, calls=calls,
    )
    common.write_artifact(args.output, artifact)
    print(f"{args.output}: {len(artifact['functions'])} functions, "
          f"{len(artifact['edges'])} edges")
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command_name", required=True)
    red = sub.add_parser("reduce", help="Xdebug profile(s) -> relative edge part")
    red.add_argument("profiles", nargs="+")
    red.add_argument("--root", required=True)
    red.add_argument("-o", "--output", required=True)
    red.set_defaults(func=cmd_reduce)
    bld = sub.add_parser("build", help="parts -> artifact")
    bld.add_argument("parts", nargs="+")
    bld.add_argument("--root", required=True)
    bld.add_argument("--project", required=True)
    bld.add_argument("--ref", required=True)
    bld.add_argument("--tool", required=True)
    bld.add_argument("--workload", required=True)
    bld.add_argument("--command", required=True)
    bld.add_argument("-o", "--output", required=True)
    bld.set_defaults(func=cmd_build)
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
