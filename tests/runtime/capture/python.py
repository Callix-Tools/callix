#!/usr/bin/env python3
"""
Record the calls a Python program makes between functions of one project, and
turn them into a runtime artifact (PEP 669 `sys.monitoring`, Python 3.12+).

    # 1. trace: run a module (like `python -m`) under the tracer, write raw edges
    python tests/runtime/capture/python.py trace --root /path/to/checkout \\
        --raw /tmp/raw.json -- -m pytest -q tests/unit_tests

    # 2. convert: raw edges -> tests/runtime/<owner>__<repo>.json
    python tests/runtime/capture/python.py convert /tmp/raw.json \\
        --root /path/to/checkout --project apache/superset --ref 6.1.0 \\
        --workload "..." --command "..." -o tests/runtime/apache__superset.json

The tracer listens to PY_START only: every time a Python function, method,
lambda, generator or coroutine STARTS (resumptions are not calls), the frame
below it is its caller. An edge is kept when both code objects live in files
under `--root` (the excludes of capture/common.py apply). Code objects of other
files are switched off the first time they start (`sys.monitoring.DISABLE`),
so the overhead is paid for project code only. A call through C (`map`,
`sorted(key=)`, a property, `type.__call__` reaching `__init__`) is attributed
to the Python frame that made it; a call from a dependency (pytest calling a
test, Flask calling a view) has no project caller and is not an edge.
Decorator wrappers are kept as executed (caller -> `deco.<locals>.wrapper` ->
decorated function), the convention of PyCG's ground truth in tests/accuracy.

Names are `co_qualname` (`Class.method`, `outer.<locals>.inner`,
`f.<locals>.<lambda>`). The line is the `def` line: for a decorated function
`co_firstlineno` is the first decorator's line, so the source is read forward
to the `def`.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import linecache
import os
import runpy
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import common  # noqa: E402

CO_OPTIMIZED = 0x0001  # set on functions; a module or class body runs without it
EXCLUDE = common.DEFAULT_EXCLUDE + (
    "superset-frontend/**", ".venv/**", "venv/**", "**/site-packages/**",
)


def _def_line(filename: str, first: int) -> int:
    """`co_firstlineno` of a decorated function is its first decorator; find the def."""
    text = linecache.getline(filename, first).lstrip()
    if not text.startswith("@"):
        return first
    for line in range(first + 1, first + 200):
        text = linecache.getline(filename, line)
        if not text:
            break
        stripped = text.lstrip()
        if stripped.startswith(("def ", "async def ", "class ")):
            return line
    return first


# Frames of these files are looked through: `with cm():` starts the generator of a
# @contextmanager function from contextlib.__enter__, but the call is written in `cm()`.
TRANSPARENT = frozenset({contextlib.__file__})


def trace(root: str, raw: str, argv: list[str]) -> int:
    monitoring = sys.monitoring
    tool = monitoring.PROFILER_ID
    monitoring.use_tool_id(tool, "callix-runtime")
    root_path = Path(root).resolve()
    known: dict = {}  # code -> key tuple or None
    edges: dict = {}
    getframe = sys._getframe
    disable = monitoring.DISABLE

    def key_of(code):
        try:
            return known[code]
        except KeyError:
            pass
        rel = common.relativize(code.co_filename, root_path, EXCLUDE)
        key = None
        if rel is not None and code.co_flags & CO_OPTIMIZED and code.co_filename.endswith(".py"):
            line = code.co_firstlineno
            if not code.co_name.startswith("<"):
                line = _def_line(code.co_filename, line)
            key = (rel, code.co_qualname, line)
        known[code] = key
        return key

    def on_start(code, _offset):
        callee = key_of(code)
        if callee is None:
            return disable
        frame = getframe(1).f_back
        while frame is not None and frame.f_code.co_filename in TRANSPARENT:
            frame = frame.f_back
        if frame is None:
            return None
        caller = key_of(frame.f_code)
        if caller is not None:
            pair = (caller, callee)
            edges[pair] = edges.get(pair, 0) + 1
        return None

    monitoring.register_callback(tool, monitoring.events.PY_START, on_start)
    monitoring.set_events(tool, monitoring.events.PY_START)
    status = 0
    here = str(Path(__file__).resolve().parent)
    sys.path[:] = [os.getcwd()] + [p for p in sys.path if p != here]
    sys.argv = argv[:]
    try:
        if argv and argv[0] == "-m":
            sys.argv = argv[1:]
            runpy.run_module(argv[1], run_name="__main__", alter_sys=True)
        else:
            runpy.run_path(argv[0], run_name="__main__")
    except SystemExit as exc:
        status = exc.code if isinstance(exc.code, int) else (0 if exc.code is None else 1)
    finally:
        monitoring.set_events(tool, 0)
        monitoring.register_callback(tool, monitoring.events.PY_START, None)
        monitoring.free_tool_id(tool)
        rows = [[*a, *b, n] for (a, b), n in edges.items()]
        rows.sort()
        Path(raw).write_text(json.dumps({"python": sys.version.split()[0], "edges": rows}))
        print(f"python.py: {len(rows)} raw edges -> {raw} (exit {status})", file=sys.stderr)
    return status


def convert(args: argparse.Namespace) -> int:
    calls = []
    version = None
    for raw in args.raw:
        data = json.loads(Path(raw).read_text(encoding="utf-8"))
        version = version or data.get("python")
        for cf, cn, cl, tf, tn, tl, count in data["edges"]:
            calls.append(((cf, cn, cl), (tf, tn, tl), count))
    tool = args.tool or f"sys.monitoring (PEP 669) PY_START, CPython {version}"
    artifact = common.build_artifact(
        project=args.project, ref=args.ref, sha=args.sha or common.git_sha(args.root),
        language="python", tool=tool, workload=args.workload,
        command=args.command, calls=calls,
    )

    common.write_artifact(args.output, artifact)
    print(f"{args.output}: {len(artifact['functions'])} functions, "
          f"{len(artifact['edges'])} edges")
    return 0


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "trace":
        parser = argparse.ArgumentParser(prog="python.py trace")
        parser.add_argument("--root", required=True)
        parser.add_argument("--raw", required=True)
        rest = sys.argv[2:]
        split = rest.index("--") if "--" in rest else len(rest)
        args = parser.parse_args(rest[:split])
        return trace(args.root, args.raw, rest[split + 1:])
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="cmd", required=True)
    conv = sub.add_parser("convert")
    conv.add_argument("raw", nargs="+")
    conv.add_argument("--root", required=True)
    conv.add_argument("--project", required=True)
    conv.add_argument("--ref", required=True)
    conv.add_argument("--sha", default="")
    conv.add_argument("--tool", default="")
    conv.add_argument("--workload", required=True)
    conv.add_argument("--command", required=True)
    conv.add_argument("-o", "--output", required=True)
    args = parser.parse_args()
    return convert(args)


if __name__ == "__main__":
    sys.exit(main())
