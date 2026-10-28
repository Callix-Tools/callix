#!/usr/bin/env python3
"""
Turn callgrind output files into a runtime artifact (C, C++ and Rust captures).

    python tests/runtime/capture/callgrind.py callgrind.out.* \\
        --root /path/to/checkout --project redis/redis --ref 8.10.0 \\
        --language c --workload "redis-server tests" --command "valgrind ..." \\
        -o tests/runtime/redis__redis.json

Only calls whose BOTH ends are files of the project (under `--root`, not
excluded) are kept. Counts are the `calls=` counts, summed over every file and
every `part:` section. Handles the compressed `fn=(id) name` form and the
uncompressed one, relative position compression (+N, -N, *) and
`--dump-instr=yes` position columns.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import common  # noqa: E402


def _resolve(spec, table):
    """`(id) name` defines, `(id)` refers, a bare name is itself."""
    spec = spec.strip()
    if spec.startswith("("):
        close = spec.index(")")
        ident = spec[1:close]
        name = spec[close + 1:].strip()
        if name:
            table[ident] = name
            return name
        return table.get(ident, "")
    return spec


def _position(tokens, columns, prev):
    """Decode position tokens against `prev`; returns the new absolute list."""
    out = []
    for index in range(columns):
        token = tokens[index] if index < len(tokens) else "*"
        before = prev[index] if index < len(prev) else 0
        if token == "*":
            out.append(before)
        elif token[0] == "+":
            out.append(before + int(token[1:], 0))
        elif token[0] == "-":
            out.append(before - int(token[1:], 0))
        else:
            out.append(int(token, 0))
    return out


def parse_text(text, fn_lines, counts):
    """Accumulate one callgrind file into `fn_lines` {(file, name): min line} and `counts`."""
    files = {}
    funcs = {}
    columns = 1
    has_line = True
    prev = [0]
    fl = ""
    cur_file = ""
    fn = ""
    callee_name = ""
    callee_file = ""
    pending = None  # set after a calls= line: the next line is its cost line

    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        head = line[0]
        if head.isdigit() or head in "+-*":
            tokens = line.split()
            if pending is not None:
                prev = _position(tokens, columns, prev)
                pending = None
                continue
            prev = _position(tokens, columns, prev)
            if has_line and fn and cur_file == fl:
                number = prev[-1]
                if number > 0:
                    key = (fl, fn)
                    old = fn_lines.get(key, 0)
                    if old == 0 or number < old:
                        fn_lines[key] = number
                    else:
                        fn_lines.setdefault(key, old)
            continue
        if line.startswith("positions:"):
            names = line.split(":", 1)[1].split()
            columns = max(len(names), 1)
            has_line = "line" in names
            prev = [0] * columns
            continue
        if line.startswith("calls="):
            parts = line[len("calls="):].split()
            count = int(parts[0])
            # The target position is relative to the call site but does not
            # replace it: the cost line that follows is relative to the call site.
            _position(parts[1:], columns, prev)
            target_file = callee_file or fl
            if fn and callee_name and fl and not callee_name.startswith("<cycle"):
                if not fn.startswith("<cycle"):
                    pair = ((fl, fn), (target_file, callee_name))
                    counts[pair] = counts.get(pair, 0) + count
            pending = (target_file, callee_name, count)
            callee_file = ""
            continue
        if "=" not in line:
            continue
        key, _, value = line.partition("=")
        if key == "fl":
            fl = _resolve(value, files)
            cur_file = fl
        elif key in ("fi", "fe"):
            cur_file = _resolve(value, files)
        elif key in ("cfi", "cfl"):
            callee_file = _resolve(value, files)
        elif key == "fn":
            fn = common.clean_name(_resolve(value, funcs))
            cur_file = fl
            if fn and fl and not fn.startswith("<cycle"):
                fn_lines.setdefault((fl, fn), 0)
        elif key == "cfn":
            callee_name = common.clean_name(_resolve(value, funcs))
    return


def _project_file(path, root, exclude):
    """`relativize`, plus: a relative path must name a file under `root`.

    Callgrind prints a relative path for objects built without a compilation
    directory (glibc's `./csu/../csu/libc-start.c`), which is never project code.
    """
    rel = common.relativize(path, root, exclude)
    if rel is not None and not Path(str(path).strip()).is_absolute():
        if not (Path(root) / rel).is_file():
            return None
    return rel


def parse(paths, root, exclude=common.DEFAULT_EXCLUDE):
    """Return `((file, name, line), (file, name, line), count)` triples for in-project calls."""
    fn_lines = {}
    counts = {}
    for path in paths:
        text = Path(path).read_text(encoding="utf-8", errors="replace")
        parse_text(text, fn_lines, counts)
    calls = []
    for ((cf, cn), (tf, tn)), count in counts.items():
        rel_caller = _project_file(cf, root, exclude)
        rel_callee = _project_file(tf, root, exclude)
        if rel_caller is None or rel_callee is None:
            continue
        calls.append((
            (rel_caller, cn, fn_lines.get((cf, cn), 0)),
            (rel_callee, tn, fn_lines.get((tf, tn), 0)),
            count,
        ))
    return calls


def _creator(path):
    for raw in Path(path).read_text(encoding="utf-8", errors="replace").splitlines():
        if raw.startswith("creator:"):
            return raw.split(":", 1)[1].strip()
    return "callgrind"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    parser.add_argument("files", nargs="+", help="callgrind.out files")
    parser.add_argument("--root", required=True, help="the project checkout")
    parser.add_argument("--project", required=True, help="owner/repo")
    parser.add_argument("--ref", required=True)
    parser.add_argument("--language", required=True, choices=["c", "cpp", "rust"])
    parser.add_argument("--workload", required=True)
    parser.add_argument("--command", required=True)
    parser.add_argument("--tool", default=None)
    parser.add_argument("--exclude", action="append", default=None, help="glob, repeatable")
    parser.add_argument("-o", "--output", required=True)
    args = parser.parse_args(argv)

    exclude = tuple(args.exclude) if args.exclude else common.DEFAULT_EXCLUDE
    calls = parse(args.files, args.root, exclude)
    artifact = common.build_artifact(
        project=args.project,
        ref=args.ref,
        sha=common.git_sha(args.root),
        language=args.language,
        tool=args.tool or _creator(args.files[0]),
        workload=args.workload,
        command=args.command,
        calls=calls,
    )
    common.write_artifact(args.output, artifact)
    print(f"{args.output}: {len(artifact['functions'])} functions, {len(artifact['edges'])} edges")
    return 0


if __name__ == "__main__":
    sys.exit(main())
