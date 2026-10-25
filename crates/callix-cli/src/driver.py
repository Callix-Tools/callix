"""Glue between the callix CLI and the adapters in `callix._core`.

The CLI embeds an interpreter and registers the native module as `_core`; this
file is compiled into the binary and run there. It is the only Python the CLI
has, it holds no logic beyond choosing adapters and merging their graphs, and it
speaks JSON in both directions so the Rust side never touches a Python object.

Every entry point takes a JSON request and returns `(graph_json, info_json)`,
where `graph_json` is empty for operations that produce no graph.
"""

import json
import sys
import time

import _core as core

#: Language name -> adapter class name, in the order graphs are merged.
ADAPTERS = [
    ("python", "PythonAdapter"),
    ("typescript", "TypeScriptAdapter"),
    ("go", "GoAdapter"),
    ("rust", "RustAdapter"),
    ("php", "PhpAdapter"),
    ("c", "CAdapter"),
    ("cpp", "CppAdapter"),
    ("yaml", "YamlAdapter"),
]

METRIC_KEYS = ("queries", "resolved", "internal", "external", "unresolved")


def _adapter(language, resolve):
    name = dict(ADAPTERS)[language]
    return getattr(core, name)(resolve=resolve)


def _detect(path):
    """The languages whose adapter finds something to analyse under `path`."""
    found = []
    for language, _ in ADAPTERS:
        # `can_handle` only looks at the root itself, so a Go module in a
        # subdirectory fails it while `collect_files` finds the sources.
        adapter = _adapter(language, False)
        try:
            if adapter.collect_files(path):
                found.append(language)
        except Exception:  # noqa: BLE001 - a detector that fails detects nothing
            continue
    return found


def _status(graph):
    value = graph.metadata.get("resolver_status")
    return str(value) if value is not None else "unknown"


def _sum_metrics(parts):
    total = {key: 0 for key in METRIC_KEYS}
    total["seconds"] = 0.0
    for metrics in parts:
        for key in METRIC_KEYS:
            total[key] += int(metrics.get(key, 0))
        total["seconds"] += float(metrics.get("seconds", 0.0))
    total["resolved_pct"] = (
        round(total["resolved"] * 100.0 / total["queries"], 1) if total["queries"] else 0.0
    )
    total["seconds"] = round(total["seconds"], 3)
    return total


def analyze(request):
    path = request["path"]
    explicit = request.get("languages")
    resolve = bool(request.get("resolve", True))
    languages = explicit if explicit else _detect(path)

    graph = None
    reports = []
    metrics = []
    for language in languages:
        started = time.monotonic()
        entry = {"language": language, "status": None, "nodes": 0, "relations": 0,
                 "seconds": 0.0, "error": None, "merged": False}
        try:
            part = _adapter(language, resolve).analyze(path)
        except Exception as err:  # noqa: BLE001 - reported per language
            entry["error"] = f"{type(err).__name__}: {err}"
            entry["seconds"] = round(time.monotonic() - started, 3)
            reports.append(entry)
            continue
        entry["status"] = _status(part)
        entry["nodes"] = len(part.nodes)
        entry["relations"] = len(part.relations)
        part_metrics = part.metadata.get("resolver_metrics")
        if isinstance(part_metrics, dict):
            metrics.append(part_metrics)
        if graph is None:
            graph = part
            entry["merged"] = True
        else:
            try:
                graph.merge(part, allow_shared=True)
                entry["merged"] = True
            except core.DuplicateNodeError as err:
                entry["error"] = f"DuplicateNodeError: {err}"
        entry["seconds"] = round(time.monotonic() - started, 3)
        reports.append(entry)

    info = {"languages": reports, "detected": languages, "empty": graph is None}
    if graph is None:
        return "", json.dumps(info)

    if metrics:
        graph.metadata["resolver_metrics"] = _sum_metrics(metrics)
    indent = request.get("indent")
    return graph.to_json(indent=indent), json.dumps(info)


def _load(path):
    with open(path, encoding="utf-8") as handle:
        return core.Graph.from_json(handle.read())


def merge(request):
    paths = request["graphs"]
    graph = _load(paths[0])
    for path in paths[1:]:
        graph.merge(_load(path), allow_shared=bool(request.get("allow_shared", True)))
    return graph.to_json(indent=request.get("indent")), json.dumps(
        {"merged": len(paths), "nodes": len(graph.nodes), "relations": len(graph.relations)}
    )


def _node(node):
    data = core.node_to_dict(node)
    return {key: data[key] for key in ("id", "kind", "qualified_name")}


def _relation(relation):
    data = core.relation_to_dict(relation)
    return {key: data[key] for key in ("source_id", "target_id", "kind")}


def diff(request):
    old, new = _load(request["old"]), _load(request["new"])
    delta = old.diff(new)
    info = {
        "is_empty": delta.is_empty,
        "added_nodes": [_node(n) for n in delta.added_nodes],
        "removed_nodes": [_node(n) for n in delta.removed_nodes],
        "changed_nodes": [_node(after) for _, after in delta.changed_nodes],
        "added_relations": [_relation(r) for r in delta.added_relations],
        "removed_relations": [_relation(r) for r in delta.removed_relations],
    }
    return "", json.dumps(info)


def environment(request):
    info = {
        "python": sys.version.split()[0],
        "base_prefix": sys.base_prefix,
        "callix": core.__version__,
        "schema_version": core.SCHEMA_VERSION,
    }
    return "", json.dumps(info)


def handle(text):
    request = json.loads(text)
    return {
        "analyze": analyze,
        "merge": merge,
        "diff": diff,
        "environment": environment,
    }[request["op"]](request)
