#!/usr/bin/env python3
"""
Turn the edge lists of the JavaScript/TypeScript probe into a runtime artifact.

    python tests/runtime/capture/jsprobe.py TRACE_DIR \\
        --root /path/to/copy --project colinhacks/zod --ref v4.4.3 --sha <commit> \\
        --tool "..." --workload "..." --command "..." -o tests/runtime/colinhacks__zod.json

The probe (`capture/jsprobe/`) writes one `<pid>-<n>.jsonl` per test file, one
line per caller/callee pair: `[[file, name, line], [file, name, line], count]`,
files relative to the project root. Pairs are summed over every file; only
calls whose BOTH ends are project files (not excluded) are kept.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import common  # noqa: E402


def read_traces(paths, root, exclude=common.DEFAULT_EXCLUDE):
    """Yield `((file, name, line), (file, name, line), count)` for project calls."""
    files = []
    for path in paths:
        path = Path(path)
        files.extend(sorted(path.glob("*.jsonl")) if path.is_dir() else [path])
    for trace in files:
        for line in trace.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            caller, callee, count = json.loads(line)
            ends = []
            for file, name, first in (caller, callee):
                rel = common.relativize(file, root, exclude)
                if rel is None:
                    break
                ends.append((rel, name, int(first)))
            else:
                yield ends[0], ends[1], int(count)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("traces", nargs="+", help="trace directories or .jsonl files")
    parser.add_argument("--root", required=True, help="the directory the trace paths are relative to")
    parser.add_argument("--project", required=True)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--sha", required=True, help="the commit of the checkout the lines belong to")
    parser.add_argument("--language", default="typescript")
    parser.add_argument("--tool", required=True)
    parser.add_argument("--workload", required=True)
    parser.add_argument("--command", required=True)
    parser.add_argument("-o", "--output", required=True)
    args = parser.parse_args()
    artifact = common.build_artifact(
        project=args.project, ref=args.ref, sha=args.sha, language=args.language,
        tool=args.tool, workload=args.workload, command=args.command,
        calls=read_traces(args.traces, args.root),
    )
    common.write_artifact(args.output, artifact)
    print(f"{args.output}: {len(artifact['functions'])} functions, {len(artifact['edges'])} edges")
    return 0


if __name__ == "__main__":
    sys.exit(main())
