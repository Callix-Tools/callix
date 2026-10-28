#!/usr/bin/env python3
"""
Score callix against call graphs RECORDED while real projects ran.

A runtime artifact (`tests/runtime/<owner>__<repo>.json`) lists the functions of
a project that a run executed and the calls between them. This script analyses
the pinned checkout with the callix binary, maps every artifact function to a
graph node and says, for each executed call, whether callix knows it.

    compare   analyse the checkouts, score, write tests/runtime/report.json
    check     fail when a project's recall is below its floor in thresholds.json
    update    raise the floors to the recall of the last compare; never lowers

A trace is a lower bound: an edge that did not execute proves nothing, so only
recall is scored. Precision stays with tests/accuracy. See README.md here.
Stdlib only; it reads the graph JSON the CLI writes and never imports callix.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE / "capture"))

import common  # noqa: E402

THRESHOLDS = HERE / "thresholds.json"
REPORT = HERE / "report.json"
EXAMPLE_CAP = 50
FUNCTION_KINDS = {"function", "method"}
ANONYMOUS = re.compile(
    r"^(?:\{\{closure\}\}|\{closure#\d+\}|<lambda>|<listcomp>|<genexpr>|<dictcomp>"
    r"|<setcomp>|<locals>|<anonymous>|)$"
)
GO_ANONYMOUS = re.compile(r"^(?:func\d+|\d+)$")
PHP_ANONYMOUS = re.compile(r"^\{closure(?::\d+(?:-\d+)?)?\}$")
PHP_IMPLICIT = frozenset({
    "__construct", "__destruct", "__call", "__callStatic", "__get", "__set",
    "__isset", "__unset", "__sleep", "__wakeup", "__serialize", "__unserialize",
    "__toString", "__invoke", "__set_state", "__clone", "__debugInfo",
    "offsetGet", "offsetSet", "offsetExists", "offsetUnset", "getIterator",
})
OPERATOR = re.compile(
    r"operator(?:\s*(?:\(\)|\[\]|->\*?|<<=?|>>=?|<=>|[-+*/%^&|~!=<>]=?|&&|\|\||\+\+|--|,))?"
)
RUST_STD_TRAIT = re.compile(r"^(?:core|std|alloc)::(?:ops|iter|fmt|cmp|hash)::")
CPP_ABI_TAG = re.compile(r"\[abi:[\w.]+\]")
CPP_COMPILER_GENERATED = re.compile(
    r"^(?:_GLOBAL__sub_[ID]_|__static_initialization_and_destruction_|__cxx_global_var_init)"
)
GTEST_MACROS = ("TEST", "TEST_F", "TEST_P", "TYPED_TEST", "TYPED_TEST_P")
# Languages `callix analyze` is run with for an artifact of this language
# (a C++ project may carry C files that call into it).
ANALYZE_LANGS = {"cpp": "cpp,c"}
TS_ANONYMOUS = re.compile(r"^<anonymous(?:#\d+)?>$")
TS_OBJECT = re.compile(r"^<object(?:#\d+)?>$")
TS_ACCESSOR = re.compile(r"^(?:get|set) ")


# ---------------------------------------------------------------------------
# Name handling
# ---------------------------------------------------------------------------


def _strip_params(text: str) -> str:
    text = text.strip()
    if text.endswith(" const"):
        text = text[: -len(" const")].rstrip()
    if not text.endswith(")"):
        return text
    depth = 0
    for index in range(len(text) - 1, -1, -1):
        char = text[index]
        if char == ")":
            depth += 1
        elif char == "(":
            depth -= 1
            if depth == 0:
                head = text[:index].rstrip()
                if head.endswith("operator"):
                    return text
                return head
    return text


def _strip_brackets(text: str, open_char: str, close_char: str) -> str:
    """Drop `<...>` (or `[...]`) by depth, keeping C++ `operator<` and friends."""
    out = []
    depth = 0
    index = 0
    while index < len(text):
        if text.startswith("operator", index) and (
            index == 0 or not (text[index - 1].isalnum() or text[index - 1] == "_")
        ):
            match = OPERATOR.match(text, index)
            if match:
                if depth == 0:
                    out.append(match.group(0))
                index = match.end()
                continue
        char = text[index]
        if char == open_char:
            depth += 1
        elif char == close_char and depth > 0:
            depth -= 1
        elif depth == 0:
            out.append(char)
        index += 1
    return "".join(out)


def _rust_qualified(raw: str) -> tuple[str, str | None, str] | None:
    """`<T as Trait>::rest` -> (T, Trait, rest); `<T>::rest` -> (T, None, rest)."""
    if not raw.startswith("<"):
        return None
    depth = 0
    for index, char in enumerate(raw):
        if char == "<":
            depth += 1
        elif char == ">":
            depth -= 1
            if depth == 0:
                inner = raw[1:index]
                rest = raw[index + 1:]
                break
    else:
        return None
    split_at = None
    depth = 0
    for position, char in enumerate(inner):
        if char == "<":
            depth += 1
        elif char == ">":
            depth -= 1
        elif depth == 0 and inner.startswith(" as ", position):
            split_at = position
            break
    if split_at is None:
        return inner, None, rest
    return inner[:split_at], inner[split_at + 4:], rest


def split_name(raw: str, language: str) -> tuple[list[str], bool]:
    """
    Split a recorded function name into components.

    Returns (components, anonymous). Trailing anonymous segments (closures,
    lambdas, Go `funcN`) are removed and set `anonymous`. The last component is
    the name, the one before it the qualifier.
    """
    text = raw.strip()
    if not text:
        return [], False
    if language == "typescript":
        return _split_typescript(text)
    if language == "go":
        text = re.sub(r"\(\*?([\w./]+)\)", r"\1", text)
        # the compiler numbers a package's init functions: `pkg.init.0`
        text = re.sub(r"\.init\.\d+(?=\.|$)", ".init", text)
    if language in ("c", "cpp", "rust"):
        text = _strip_params(text) if language != "rust" else text.strip()
    if language == "cpp":
        text = CPP_ABI_TAG.sub("", text)
    if language == "rust":
        while True:
            parts = _rust_qualified(text)
            if parts is None:
                break
            holder, _trait, rest = parts
            holder = re.sub(r"^(?:&\s*(?:mut\s+)?|dyn\s+)", "", holder.strip())
            text = holder + rest
    if language in ("c", "cpp", "rust"):
        text = _strip_brackets(text, "<", ">")
    if language == "go":
        text = _strip_brackets(text, "[", "]")
    if language in ("c", "cpp", "rust"):
        components = text.split("::")
    elif language == "php":
        head = text
        if "{closure" in text:
            head = text[: text.index("{closure")]
            cut = max(head.rfind("->"), head.rfind("::"))
            if cut >= 0:
                head = head[:cut]
        components = re.split(r"::|->|\\", head)
        if "{closure" in text:
            components.append("{closure}")
    else:
        components = text.split(".")
    if language == "python":
        components = [c for c in components if c != "<locals>"]
    anonymous = False
    while components:
        last = components[-1]
        if (
            ANONYMOUS.match(last)
            or (language == "go" and GO_ANONYMOUS.match(last))
            or (language == "php" and PHP_ANONYMOUS.match(last))
        ):
            components.pop()
            anonymous = True
        else:
            break
    return [c for c in components if c != ""] if components else [], anonymous


def _split_typescript(text: str) -> tuple[list[str], bool]:
    """`scope.name`, with `<anonymous#N>` closures and `<object#N>.key` members.

    A member of an object literal is a closure to callix (it is no node), so
    everything from the `<object>` segment on is dropped and the function maps
    by position, like an anonymous one. An accessor `get x` is the METHOD `x`.
    """
    components = text.split(".")
    anonymous = False
    for index, part in enumerate(components):
        if TS_OBJECT.match(part):
            components = components[:index]
            anonymous = True
            break
    while components and TS_ANONYMOUS.match(components[-1]):
        components.pop()
        anonymous = True
    if components:
        components[-1] = TS_ACCESSOR.sub("", components[-1])
    return [c for c in components if c], anonymous


def is_implicit(raw: str, language: str) -> bool:
    """True when the callee is called by the language, not written at a call site.

    The per-language rules are deliberately small; the per-language capture
    issues extend them (and tests/runtime/README.md) when a capture shows more.
    """
    if language == "typescript":
        last = raw.strip().split(".")[-1]
        return last == "constructor" or TS_ACCESSOR.match(last) is not None
    components, anonymous = split_name(raw, language)
    if not components:
        return False
    name = components[-1]
    qualifier = components[-2] if len(components) > 1 else None
    if language == "cpp":
        return (
            CPP_COMPILER_GENERATED.match(name) is not None
            or name.startswith("~")
            or name.startswith("operator")
            or (qualifier is not None and name == qualifier)
        )
    if language == "rust":
        if name in ("drop", "drop_in_place") and (
            "Drop" in raw or "ptr" in components[:-1]
        ):
            return True
        parts = _rust_qualified(raw.strip())
        if parts is not None and parts[1] is not None:
            return RUST_STD_TRAIT.match(parts[1].strip()) is not None
        return False
    if language == "python":
        # `A()` is written as a call and scored as one to `A.__init__` (see Graph)
        return (
            len(name) > 4 and name.startswith("__") and name.endswith("__")
            and name != "__init__"
        )
    if language == "php":
        return name in PHP_IMPLICIT
    if language == "go":
        # `pkg.init.funcN` is a closure in a package-level var initializer
        return name == "init" and not anonymous
    return False


PYTHON_ACCESSORS = re.compile(
    r"^(?:.*\.)?(?:property|cached_property|hybrid_property|memoized_property"
    r"|lazy_property|declared_attr|setter|getter|deleter|expression)$"
)


def is_implicit_node(node: dict, language: str) -> bool:
    """True when the callee node is run by attribute access (a Python property)."""
    if language != "python":
        return False
    meta = node.get("metadata") or {}
    return bool(meta.get("is_property")) or any(
        PYTHON_ACCESSORS.match(d) for d in meta.get("decorators") or []
    )


# ---------------------------------------------------------------------------
# Graph index and mapping
# ---------------------------------------------------------------------------


def _span(node) -> tuple[int, int, int, int] | None:
    span = node.get("span")
    if isinstance(span, dict):
        span = span.get("__span__")
    if not span or len(span) != 4:
        return None
    return tuple(span)


def _declared_files(node) -> set[str]:
    files = set()
    meta = node.get("metadata") or {}
    for entry in meta.get("declared_at") or []:
        if isinstance(entry, dict) and entry.get("file"):
            files.add(entry["file"])
    if meta.get("declared_in"):
        value = meta["declared_in"]
        files.update([value] if isinstance(value, str) else list(value))
    return files


class Graph:
    """A callix graph indexed for mapping: callables by file, CALLS by source."""

    def __init__(self, data: dict) -> None:
        self.nodes = {n["id"]: n for n in data["nodes"]}
        self.files: set[str] = set()
        self.by_file_name: dict[tuple[str, str], list[dict]] = collections.defaultdict(list)
        self.containers: dict[str, list[dict]] = collections.defaultdict(list)
        self.inits: dict[str, list[dict]] = collections.defaultdict(list)
        self.file_nodes: dict[str, str] = {}
        for node in data["nodes"]:
            path = node.get("file_path")
            if path:
                self.files.add(path)
            if path and node["kind"] == "file":
                self.file_nodes[path] = node["id"]
            if node["kind"] in FUNCTION_KINDS and path:
                for file in {path} | _declared_files(node):
                    self.by_file_name[(file, node["name"])].append(node)
            if node["kind"] == "function" and node["name"] == "init" and path:
                self.inits[path.rpartition("/")[0]].append(node)
            if path and _span(node) and (
                node["kind"] in FUNCTION_KINDS or node["kind"] == "variable"
            ):
                self.containers[path].append(node)
        self.calls: dict[str, dict[str, str]] = collections.defaultdict(dict)
        self.called_names: dict[str, set[str]] = collections.defaultdict(set)
        initializers = self._initializers(data)
        for rel in data["relations"]:
            if rel["kind"] != "calls":
                continue
            src, dst = rel["source_id"], rel["target_id"]
            resolution = (rel.get("metadata") or {}).get("resolution", "unknown")
            self.calls[src].setdefault(dst, resolution)
            if dst in initializers:
                # Python `A()` runs `A.__init__`, its own or the nearest base's;
                # the accuracy harness reads a CALLS to a class the same way
                self.calls[src].setdefault(initializers[dst], resolution)
            target = self.nodes.get(dst)
            if target is not None:
                self.called_names[src].update(self.names_of(target))

    @staticmethod
    def _initializers(data: dict) -> dict[str, str]:
        """Class id -> the `__init__` method it runs (own, else inherited)."""
        own = {}
        for node in data["nodes"]:
            if node["kind"] == "method" and node["name"] == "__init__":
                holder = node["qualified_name"].rsplit(".", 1)[0]
                own.setdefault(holder, node["id"])
        classes = {n["id"]: n["qualified_name"] for n in data["nodes"] if n["kind"] == "class"}
        bases = collections.defaultdict(list)
        for rel in data["relations"]:
            if rel["kind"] == "inherits_from" and rel["source_id"] in classes:
                bases[rel["source_id"]].append(rel["target_id"])

        def find(class_id: str, seen: set) -> str | None:
            if class_id in seen or class_id not in classes:
                return None
            seen.add(class_id)
            found = own.get(classes[class_id])
            if found is not None:
                return found
            for base in bases.get(class_id, []):
                found = find(base, seen)
                if found is not None:
                    return found
            return None

        result = {}
        for class_id in classes:
            init = find(class_id, set())
            if init is not None:
                result[class_id] = init
        return result

    @staticmethod
    def names_of(node: dict) -> set[str]:
        if node["kind"] != "external_symbol":
            return {node["name"]}
        base = node["qualified_name"].split("@")[0]
        names = {base}
        names.add(re.split(r"\.|::|\\", base)[-1])
        return names


def load_graph(path: Path | str) -> Graph:
    return Graph(json.loads(Path(path).read_text(encoding="utf-8")))


def _area(node: dict) -> tuple[int, int]:
    s = _span(node)
    return (s[2] - s[0], s[3] - s[1])


def _contains(node: dict, line: int) -> bool:
    s = _span(node)
    return s is not None and s[0] <= line <= s[2]


def gtest_class(qualified_name: str) -> str | None:
    """`ns::TEST_F(Suite,Name)` -> `Suite_Name_Test`, the class gtest generates."""
    match = re.search(r"\b(?:TYPED_)?TEST(?:_F|_P)?\(\s*(\w+)\s*,\s*(\w+)\s*\)$", qualified_name)
    return f"{match.group(1)}_{match.group(2)}_Test" if match else None


def analyze_langs(language: str) -> str:
    """The `--lang` value `callix analyze` gets for an artifact of `language`."""
    return ANALYZE_LANGS.get(language, language)


def map_function(fn: dict, graph: Graph, language: str) -> tuple[str | None, str | None]:
    """Map an artifact function to a node id; returns (id, None) or (None, reason)."""
    components, anonymous = split_name(fn["name"], language)
    file = fn["file"]
    line = fn.get("line", 0) or 0
    if language == "typescript" and (anonymous or not components) and line > 0:
        # callix encloses a closure by the innermost callable and top-level code,
        # module-level initializers included, by the FILE (never by a VARIABLE)
        inside = [
            c for c in graph.containers.get(file, [])
            if c["kind"] in FUNCTION_KINDS and _contains(c, line)
        ]
        if inside:
            return min(inside, key=_area)["id"], None
        if file in graph.file_nodes:
            return graph.file_nodes[file], None
    candidates: list[dict] = []
    if language == "python" and anonymous and line > 0:
        # a lambda is a node of its own (`<lambdaN>`); a generator expression is
        # not, and folds into the callable around it, never into a variable
        inside = [
            c for c in graph.containers.get(file, [])
            if c["kind"] in FUNCTION_KINDS and _contains(c, line)
        ]
        if inside:
            return min(inside, key=_area)["id"], None
    if components:
        name = components[-1]
        qualifier = components[-2] if len(components) > 1 else None
        candidates = list(graph.by_file_name.get((file, name), []))
        if language == "cpp" and name == "TestBody" and qualifier and qualifier.endswith("_Test"):
            candidates = [
                c for macro in GTEST_MACROS
                for c in graph.by_file_name.get((file, macro), [])
                if gtest_class(c["qualified_name"]) == qualifier
            ]

        if not candidates and language == "rust":
            # A raw identifier: the symbol says `async`, the source `r#async`.
            candidates = list(graph.by_file_name.get((file, "r#" + name), []))

        if language == "go" and name == "init":
            if anonymous:
                # a var-initializer closure: only its line places it
                candidates = []
            elif not candidates:
                # callix keeps ONE init node per package, in the first file
                candidates = list(graph.inits.get(file.rpartition("/")[0], []))
        if len(candidates) > 1:
            if line > 0:
                inside = [
                    c for c in candidates if c.get("file_path") == file and _contains(c, line)
                ]
                if inside:
                    candidates = inside
            if len(candidates) > 1 and line > 0 and all(_span(c) for c in candidates):
                ordered = sorted(candidates, key=_area)
                if _area(ordered[0]) < _area(ordered[1]) and _contains(ordered[0], line):
                    candidates = ordered[:1]
            if len(candidates) > 1 and qualifier:
                matching = [
                    c for c in candidates
                    if qualifier in re.split(r"[:.\\/]+", c["qualified_name"])
                ]
                if matching:
                    candidates = matching
            if len(candidates) > 1:
                return None, "ambiguous"
        if len(candidates) == 1:
            return candidates[0]["id"], None
    if language == "rust" and not candidates and line > 0:
        # A function a `macro_rules!` of the project generates (`$name` in the
        # macro body) has no name in the source; its calls are written in the
        # macro, which callix makes a FUNCTION with metadata["macro"].
        inside = [
            c for c in graph.containers.get(file, [])
            if (c.get("metadata") or {}).get("macro") and _contains(c, line)
        ]
        if inside:
            return min(inside, key=_area)["id"], None
    if (anonymous or not components) and line > 0:
        inside = [c for c in graph.containers.get(file, []) if _contains(c, line)]
        if inside:
            return min(inside, key=_area)["id"], None
    return None, ("no_name" if file in graph.files else "no_file")


# ---------------------------------------------------------------------------
# Scoring
# ---------------------------------------------------------------------------

CATEGORIES = ("found", "wrong_target", "missing", "unmapped", "implicit", "folded")


def score(artifact: dict, graph: Graph) -> dict:
    language = artifact["language"]
    functions = {f["id"]: f for f in artifact["functions"]}
    mapping = {fid: map_function(f, graph, language) for fid, f in functions.items()}

    unmapped_functions: dict[str, list[str]] = collections.defaultdict(list)
    for fid, (node_id, reason) in sorted(mapping.items()):
        if node_id is None:
            fn = functions[fid]
            unmapped_functions[reason].append(f"{fn['file']}:{fn['name']}")

    pairs: dict[tuple, dict] = {}
    for caller, callee, count in artifact["edges"]:
        key_a = mapping[caller][0] or ("raw", caller)
        key_b = mapping[callee][0] or ("raw", callee)
        entry = pairs.get((key_a, key_b))
        if entry is None:
            entry = {
                "count": 0, "caller": caller, "callee": callee,
                "implicit": True, "same": False, "diff": False,
            }
            pairs[(key_a, key_b)] = entry
        entry["count"] += count
        entry["implicit"] = (
            entry["implicit"]
            or is_implicit(functions[callee]["name"], language)
            or (key_b[0] != "raw" and is_implicit_node(graph.nodes[key_b], language))
        )

        # implicit only when every call folded into the pair is: an accessor
        # mapped to its enclosing function must not hide a call written to it
        if not is_implicit(functions[callee]["name"], language):
            if entry["implicit"]:
                entry["caller"], entry["callee"] = caller, callee
            entry["implicit"] = False
        if caller == callee:
            entry["same"] = True
        else:
            entry["diff"] = True

    totals = dict.fromkeys(CATEGORIES, 0)
    weighted = dict.fromkeys(CATEGORIES, 0)
    by_resolution: dict[str, int] = collections.defaultdict(int)
    implicit_found = 0
    examples: dict[str, list[str]] = {c: [] for c in CATEGORIES}

    for (key_a, key_b), entry in pairs.items():
        raw_a, raw_b = functions[entry["caller"]], functions[entry["callee"]]
        label = f"{raw_a['file']}:{raw_a['name']} -> {raw_b['file']}:{raw_b['name']} x{entry['count']}"
        mapped = key_a[0] != "raw" and key_b[0] != "raw"
        resolution = graph.calls.get(key_a, {}).get(key_b) if mapped else None
        if entry["implicit"]:
            category = "implicit"
            if resolution is not None:
                implicit_found += 1
        elif not mapped:
            category = "unmapped"
        elif key_a == key_b and entry["diff"] and not entry["same"]:
            category = "folded"
        elif resolution is not None:
            category = "found"
            by_resolution[resolution] += 1
        elif graph.names_of(graph.nodes[key_b]) & graph.called_names.get(key_a, set()):
            category = "wrong_target"
        else:
            category = "missing"
        totals[category] += 1
        weighted[category] += entry["count"]
        examples[category].append(label)

    def recall(table: dict) -> float | None:
        denominator = table["found"] + table["wrong_target"] + table["missing"]
        return round(table["found"] / denominator, 4) if denominator else None

    return {
        "project": artifact["project"],
        "language": language,
        "functions": len(functions),
        "functions_mapped": sum(1 for n, _ in mapping.values() if n is not None),
        "unmapped_functions": {
            reason: {"count": len(names), "examples": sorted(names)[:EXAMPLE_CAP]}
            for reason, names in sorted(unmapped_functions.items())
        },
        "edges": len(artifact["edges"]),
        "pairs": len(pairs),
        **totals,
        "implicit_found": implicit_found,
        "found_by_resolution": dict(sorted(by_resolution.items())),
        "calls": weighted,
        "recall": recall(totals),
        "recall_by_calls": recall(weighted),
        "examples": {c: sorted(v)[:EXAMPLE_CAP] for c, v in examples.items()},
    }


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------


def _artifacts(only: str | None) -> list[tuple[Path, dict]]:
    found = []
    for path in sorted(HERE.glob("*__*.json")):
        artifact = common.load_artifact(path)
        if only and artifact["project"] != only:
            continue
        found.append((path, artifact))
    return found


def _callix(explicit: str | None) -> str:
    if explicit:
        return explicit
    built = ROOT / "target" / "release" / "callix"
    if built.exists():
        return str(built)
    return os.environ.get("CALLIX") or shutil.which("callix") or "callix"


def _load_thresholds() -> dict:
    if not THRESHOLDS.exists():
        return {}
    return json.loads(THRESHOLDS.read_text(encoding="utf-8"))


def _fmt(value: float | None) -> str:
    return "-" if value is None else f"{value:.4f}"


def cmd_compare(args: argparse.Namespace) -> int:
    artifacts = _artifacts(args.project)
    if not artifacts:
        print("no runtime artifacts yet")
        return 0
    sys.path.insert(0, str(ROOT / "tests"))
    import baseline  # noqa: PLC0415

    callix = _callix(args.callix)
    version = subprocess.run(
        [callix, "--version"], capture_output=True, text=True, check=False
    ).stdout.strip()
    workdir = Path(args.workdir)
    (workdir / "runtime").mkdir(parents=True, exist_ok=True)
    results = []
    failed = False
    for _path, artifact in artifacts:
        name = artifact["project"]
        listed = baseline.projects(name)
        project = listed[0] if listed else {
            "name": name, "repo": f"https://github.com/{name}", "ref": artifact["ref"],
        }
        if str(project["ref"]) != str(artifact["ref"]):
            print(f"  warning  {name}: artifact ref {artifact['ref']} differs from "
                  f"the manifest's {project['ref']}; using the artifact's")
            project = {**project, "ref": artifact["ref"]}
        slug = baseline.slug(name)
        dest = workdir / slug
        try:
            if args.clone:
                baseline.clone(project, dest)
            if not dest.exists():
                raise RuntimeError(f"no checkout at {dest} (pass --clone)")
            head = common.git_sha(dest)
            if head != artifact["sha"]:
                raise RuntimeError(
                    f"checkout is at {head or 'unknown'}, the artifact was recorded at "
                    f"{artifact['sha']}; lines would shift"
                )
            out = workdir / "runtime" / f"{slug}.graph.json"
            subprocess.run(
                [callix, "analyze", str(dest), "--lang", analyze_langs(artifact["language"]),
                 "-q", "-o", str(out)],
                check=True,
            )
            results.append(score(artifact, load_graph(out)))
        except (RuntimeError, subprocess.CalledProcessError, OSError) as exc:
            failed = True
            print(f"  ERROR    {name}: {exc}")

    header = (
        f"{'project':<24} {'lang':<5} {'fns':>11} {'edges':>7} {'found':>6} "
        f"{'wrong':>6} {'miss':>6} {'unmap':>6} {'impl':>6} {'fold':>5} {'recall':>7}"
    )
    print(header)
    for r in results:
        print(
            f"{r['project']:<24} {r['language']:<5} "
            f"{r['functions_mapped']:>5}/{r['functions']:<5} {r['pairs']:>7} "
            f"{r['found']:>6} {r['wrong_target']:>6} {r['missing']:>6} "
            f"{r['unmapped']:>6} {r['implicit']:>6} {r['folded']:>5} {_fmt(r['recall']):>7}"
        )
        if r["found_by_resolution"]:
            print("    found by resolution: " + ", ".join(
                f"{k} {v}" for k, v in r["found_by_resolution"].items()))
        if args.verbose:
            for category in ("wrong_target", "missing", "unmapped"):
                for example in r["examples"][category]:
                    print(f"    {category}: {example}")
            for reason, info in r["unmapped_functions"].items():
                for example in info["examples"]:
                    print(f"    unmapped function ({reason}): {example}")
    REPORT.write_text(
        json.dumps({"callix": version, "projects": results}, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return 1 if failed else 0


def _report_projects(args: argparse.Namespace) -> tuple[list[dict] | None, list[str]]:
    artifacts = _artifacts(args.project)
    names = [a["project"] for _, a in artifacts]
    if not names:
        return None, []
    if not REPORT.exists():
        print("tests/runtime/report.json is missing; run `runtime.py compare` first")
        raise SystemExit(1)
    report = json.loads(REPORT.read_text(encoding="utf-8"))
    return [p for p in report["projects"] if p["project"] in names], names


def cmd_check(args: argparse.Namespace) -> int:
    projects, names = _report_projects(args)
    if projects is None:
        print("no runtime artifacts yet")
        return 0
    thresholds = _load_thresholds()
    ok = True
    reported = {p["project"] for p in projects}
    for name in names:
        if name not in reported:
            ok = False
            print(f"  FAIL     {name}: not in report.json; run `runtime.py compare`")
    for p in projects:
        floor = thresholds.get(p["project"], {}).get("recall")
        recall = p["recall"] if p["recall"] is not None else 0.0
        if floor is None:
            ok = False
            print(f"  FAIL     {p['project']}: no floor; run `runtime.py update`")
        elif recall < floor:
            ok = False
            print(f"  FAIL     {p['project']}: recall {recall:.4f} is below the floor {floor:.4f}")
        else:
            print(f"  ok       {p['project']}: recall {recall:.4f} (floor {floor:.4f})")
    return 0 if ok else 1


def raise_floors(thresholds: dict, projects: list[dict]) -> dict:
    """Return thresholds with every floor raised to the measured recall; never lowered."""
    updated = {k: dict(v) for k, v in thresholds.items()}
    for p in projects:
        if p["recall"] is None:
            continue
        old = updated.get(p["project"], {}).get("recall", 0.0)
        updated[p["project"]] = {"recall": max(old, p["recall"])}
    return updated


def cmd_update(args: argparse.Namespace) -> int:
    projects, _names = _report_projects(args)
    if projects is None:
        print("no runtime artifacts yet")
        return 0
    before = _load_thresholds()
    after = raise_floors(before, projects)
    THRESHOLDS.write_text(
        json.dumps(after, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    for name in sorted(after):
        old = before.get(name, {}).get("recall")
        new = after[name]["recall"]
        if old != new:
            print(f"  raised   {name}: {_fmt(old)} -> {_fmt(new)}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    shared = argparse.ArgumentParser(add_help=False)
    shared.add_argument("--project", help="only this owner/repo")
    shared.add_argument("--workdir", default="/tmp/callix-baseline")
    shared.add_argument("--callix", help="the callix binary (default: target/release/callix)")
    shared.add_argument("--clone", action="store_true", help="clone missing checkouts (compare)")
    shared.add_argument("-v", "--verbose", action="store_true")
    sub.add_parser("compare", parents=[shared], help="analyse, score, write report.json")
    sub.add_parser("check", parents=[shared], help="fail below the recorded floors")
    sub.add_parser("update", parents=[shared], help="raise the floors, never lower them")
    args = parser.parse_args()
    return {"compare": cmd_compare, "check": cmd_check, "update": cmd_update}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
