"""Shared helpers for the runtime-artifact capture scripts (stdlib only).

An artifact is one JSON file per project, `tests/runtime/<owner>__<repo>.json`,
holding the functions of the project that a recorded run executed and the
calls between them. See `tests/runtime/README.md` for the format.
"""

from __future__ import annotations

import fnmatch
import json
import re
import subprocess
from pathlib import Path, PurePosixPath

SCHEMA = 1
KEY_ORDER = (
    "schema", "project", "ref", "sha", "language", "tool",
    "workload", "command", "functions", "edges",
)
DEFAULT_EXCLUDE = (
    "deps/**", "vendor/**", "third_party/**",
    "node_modules/**", "target/**", ".git/**",
)

_RUST_HASH = re.compile(r"::h[0-9a-f]{16}$")
_RECURSION = re.compile(r"'\d+$")


def relativize(path, root, exclude=DEFAULT_EXCLUDE):
    """Posix path of `path` relative to `root`, or None when it is not project code."""
    text = str(path).strip()
    if not text or text.startswith("???"):
        return None
    candidate = Path(text)
    if candidate.is_absolute():
        try:
            rel = candidate.resolve().relative_to(Path(root).resolve())
        except ValueError:
            return None
    else:
        rel = candidate
    parts = PurePosixPath(rel.as_posix()).parts
    if not parts or ".." in parts:
        return None
    posix = "/".join(parts)
    for pattern in exclude:
        if fnmatch.fnmatchcase(posix, pattern):
            return None
    return posix


def clean_name(name):
    """Strip a Rust legacy hash, a callgrind recursion suffix and whitespace."""
    name = name.strip()
    name = _RECURSION.sub("", name)
    name = _RUST_HASH.sub("", name)
    return name.strip()


def build_artifact(*, project, ref, sha, language, tool, workload, command, calls):
    """
    Build an artifact from `((file, name, line), (file, name, line), count)` triples.

    Duplicate caller/callee pairs are summed. A function keeps the smallest
    line above zero it was seen with (0 when none). Ids follow the sorted
    (file, name) order and edges are sorted.
    """
    lines = {}
    counts = {}
    for caller, callee, count in calls:
        for file, name, line in (caller, callee):
            key = (file, name)
            old = lines.get(key, 0)
            if line and line > 0 and (old == 0 or line < old):
                lines[key] = line
            else:
                lines.setdefault(key, old)
        pair = (caller[0], caller[1], callee[0], callee[1])
        counts[pair] = counts.get(pair, 0) + int(count)
    keys = sorted(lines)
    ids = {key: index for index, key in enumerate(keys)}
    functions = [
        {"id": ids[key], "file": key[0], "name": key[1], "line": lines[key]}
        for key in keys
    ]
    edges = sorted(
        [ids[(cf, cn)], ids[(tf, tn)], count]
        for (cf, cn, tf, tn), count in counts.items()
    )
    return {
        "schema": SCHEMA,
        "project": project,
        "ref": ref,
        "sha": sha,
        "language": language,
        "tool": tool,
        "workload": workload,
        "command": command,
        "functions": functions,
        "edges": edges,
    }


def validate(artifact):
    """Return a list of problems; empty means the artifact is well formed."""
    problems = []
    if not isinstance(artifact, dict):
        return ["artifact is not an object"]
    if artifact.get("schema") != SCHEMA:
        problems.append(f"schema must be {SCHEMA}")
    for key in KEY_ORDER:
        if key not in artifact:
            problems.append(f"missing key {key}")
    if problems:
        return problems
    functions = artifact["functions"]
    seen = set()
    for index, fn in enumerate(functions):
        if fn.get("id") != index:
            problems.append(f"function {index} has id {fn.get('id')}")
        file = fn.get("file", "")
        if (
            not file
            or file.startswith("/")
            or "\\" in file
            or ".." in file.split("/")
        ):
            problems.append(f"function {index} has a bad file {file!r}")
        key = (file, fn.get("name"))
        if key in seen:
            problems.append(f"duplicate function {key}")
        seen.add(key)
    count = len(functions)
    previous = None
    for edge in artifact["edges"]:
        if len(edge) != 3:
            problems.append(f"edge {edge} is not [caller, callee, count]")
            continue
        caller, callee, calls = edge
        if not (0 <= caller < count and 0 <= callee < count):
            problems.append(f"edge {edge} has an invalid id")
        if calls < 1:
            problems.append(f"edge {edge} has a count below 1")
        if previous is not None and [caller, callee] <= previous:
            problems.append(f"edge {edge} is unsorted or duplicated")
        previous = [caller, callee]
    return problems


def _dumps(value):
    return json.dumps(value, ensure_ascii=False, separators=(", ", ": "))


def dump(artifact):
    """Deterministic text: header keys, one function and one edge per line."""
    out = ["{"]
    keys = [k for k in KEY_ORDER if k in artifact]
    for position, key in enumerate(keys):
        tail = "," if position < len(keys) - 1 else ""
        value = artifact[key]
        if key in ("functions", "edges"):
            if not value:
                out.append(f'"{key}": []{tail}')
                continue
            out.append(f'"{key}": [')
            for index, item in enumerate(value):
                comma = "," if index < len(value) - 1 else ""
                out.append("  " + _dumps(item) + comma)
            out.append("]" + tail)
        else:
            out.append(f'"{key}": {_dumps(value)}{tail}')
    out.append("}")
    return "\n".join(out) + "\n"


def write_artifact(path, artifact):
    problems = validate(artifact)
    if problems:
        raise ValueError("invalid artifact: " + "; ".join(problems[:5]))
    Path(path).write_text(dump(artifact), encoding="utf-8")


def load_artifact(path):
    artifact = json.loads(Path(path).read_text(encoding="utf-8"))
    problems = validate(artifact)
    if problems:
        raise ValueError(f"{path}: " + "; ".join(problems[:5]))
    return artifact


def git_sha(root):
    result = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        capture_output=True, text=True, check=False,
    )
    return result.stdout.strip() if result.returncode == 0 else ""
