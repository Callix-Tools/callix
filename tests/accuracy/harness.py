"""Measures what a resolver gets right, against ground truth written by hand.

The golden files and the baselines freeze what callix produces; they cannot say
whether it is correct. This does: every case is a small program together with
`callgraph.json`, the call graph a reader of the source would draw
(`{caller: [callee]}`), and a case is scored by comparing the graph callix builds
with that file.

Scoring follows the PyCG artifact (scripts/micro_benchmark.py):

    sound     every expected edge is in the output
    complete  every output edge whose caller the expectation lists is expected

and adds edge-level recall and precision, which move when a case does not flip.

What is scored, and what is deliberately not:

* Only `CALLS` edges to a definition of the program. A callee outside it (the
  standard library, a dependency) has no name in the graph, so it is counted
  separately and matched by its last name segment - a lenient rule that is
  reported on its own line and never mixed into the headline.
* A call that lands on a variable or a parameter (`a = f; a()`) did not find a
  callee. It is not scored as a wrong edge, but it is counted and reported: a
  resolver that answers "it is `a`" has not answered the question.
"""

from __future__ import annotations

import json
import os
import re
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

import callix

ROOT = Path(__file__).resolve().parent
SUITES = ROOT / "suites"

_CONSTRUCTOR = {"python": ".__init__", "typescript": ".constructor", "php": "\\__construct"}

# A struct or class used as a value is not a call: no ground truth lists one.
_CLASS_IS_NOT_A_CALL = {"rust", "go"}

_IDENT = re.compile(r"[A-Za-z_][A-Za-z_0-9]*(?:\s*(?:\.|::|->)\s*[A-Za-z_][A-Za-z_0-9]*)*")


@dataclass
class Case:
    suite: str
    category: str
    name: str
    root: Path
    expected: dict[str, list[str]]


@dataclass
class Outcome:
    case: Case
    sound: bool = False
    complete: bool = False
    missing: list[tuple[str, str]] = field(default_factory=list)
    extra: list[tuple[str, str]] = field(default_factory=list)
    expected_edges: int = 0
    found_edges: int = 0
    produced_edges: int = 0
    correct_edges: int = 0
    to_value: int = 0  # calls that landed on a variable or a parameter
    external_expected: int = 0
    external_found: int = 0
    error: str | None = None


@dataclass
class Score:
    suite: str
    cases: int = 0
    sound: int = 0
    complete: int = 0
    expected_edges: int = 0
    found_edges: int = 0
    produced_edges: int = 0
    correct_edges: int = 0
    to_value: int = 0
    external_expected: int = 0
    external_found: int = 0
    errors: int = 0
    by_category: dict[str, list[int]] = field(default_factory=dict)  # [cases, sound, complete]

    @property
    def recall(self) -> float:
        return self.found_edges / self.expected_edges if self.expected_edges else 1.0

    @property
    def precision(self) -> float:
        return self.correct_edges / self.produced_edges if self.produced_edges else 1.0

    def metrics(self) -> dict[str, float]:
        return {
            "sound": self.sound,
            "complete": self.complete,
            "recall": round(self.recall, 4),
            "precision": round(self.precision, 4),
        }


def discover(suite_dir: Path) -> list[Case]:
    """Cases are `<suite>/<category>/<case>/callgraph.json`."""
    name = f"{suite_dir.parent.name}/{suite_dir.name}"
    cases = []
    for category in sorted(p for p in suite_dir.iterdir() if p.is_dir()):
        for case in sorted(p for p in category.iterdir() if p.is_dir()):
            truth = case / "callgraph.json"
            if truth.is_file():
                cases.append(Case(name, category.name, case.name, case, json.loads(truth.read_text())))
    return cases


def suites() -> list[Path]:
    return sorted(p for language in SUITES.iterdir() if language.is_dir() for p in language.iterdir() if p.is_dir())


# --- the graph, as the ground truth writes it -------------------------------------------


def _module_of(path: str) -> str:
    stem = path[:-3] if path.endswith(".py") else path
    module = stem.replace("/", ".")
    return module[: -len(".__init__")] if module.endswith(".__init__") else module


def _callee_text(root: Path, file: str | None, span) -> str | None:
    """The identifier chain at a call site, for callees the graph cannot name."""
    if not file or span is None:
        return None
    try:
        line = (root / file).read_text(encoding="utf-8").split("\n")[span.start_line - 1]
    except (OSError, IndexError):
        return None
    found = _IDENT.match(line[span.start_col - 1 :])
    return re.sub(r"\s+", "", found.group(0)) if found else None


@dataclass
class CallGraph:
    internal: dict[str, set[str]]
    external: dict[str, set[str]]  # caller -> identifier text at each external call site
    to_value: dict[str, set[str]]  # caller -> calls that landed on a variable or parameter


def call_graph(graph: callix.Graph, root: Path, language: str) -> CallGraph:
    nodes = graph.nodes
    by_name = {}
    for node in nodes.values():
        by_name.setdefault(node.qualified_name, node)
    bases = defaultdict(list)
    for relation in graph.relations:
        if str(relation.kind).endswith("INHERITS_FROM"):
            bases[relation.source_id].append(relation.target_id)

    def initializer(class_id: str, seen: set[str] | None = None) -> str | None:
        seen = seen if seen is not None else set()
        if class_id in seen:
            return None
        seen.add(class_id)
        found = by_name.get(nodes[class_id].qualified_name + _CONSTRUCTOR[language])
        if found is not None:
            return found.qualified_name
        for base in bases.get(class_id, []):
            inherited = initializer(base, seen)
            if inherited:
                return inherited
        return None

    result = CallGraph(defaultdict(set), defaultdict(set), defaultdict(set))
    for relation in graph.relations:
        if not str(relation.kind).endswith("CALLS"):
            continue
        source, target = nodes[relation.source_id], nodes[relation.target_id]
        source_kind, target_kind = str(source.kind), str(target.kind)
        if source_kind.endswith("FILE") and language == "python":
            caller = _module_of(source.file_path or source.qualified_name)
        else:
            caller = source.qualified_name
        if target_kind.endswith("EXTERNAL_SYMBOL"):
            span = relation.metadata.get("span")
            file = relation.metadata.get("file") or source.file_path
            result.external[caller].add(_callee_text(root, file, span) or "")
        elif target_kind.endswith(("VARIABLE", "PARAMETER", "ATTRIBUTE")):
            result.to_value[caller].add(target.qualified_name)
        elif target_kind.endswith("CLASS") and language in _CLASS_IS_NOT_A_CALL:
            continue
        elif target_kind.endswith("CLASS") and language in _CONSTRUCTOR:
            # No edge for `A()` unless `A` defines a constructor: that is what a
            # reader draws, and what PyCG's ground truth does.
            init = initializer(target.id)
            if init:
                result.internal[caller].add(init)
        else:
            result.internal[caller].add(target.qualified_name)
    return result


_SOURCE_SUFFIXES = {
    "python": (".py",),
    "typescript": (".ts", ".tsx", ".mts", ".cts"),
}  # languages whose modules are files; elsewhere the ground truth's keys say what is the program


def _program_modules(root: Path, language: str) -> set[str]:
    """Names of the modules the program is made of, as the ground truth spells them."""
    suffixes = _SOURCE_SUFFIXES[language]
    modules = set()
    for directory, dirs, names in os.walk(root):
        dirs[:] = [d for d in dirs if d != "node_modules"]
        for name in names:
            if not name.endswith(suffixes) or name.endswith(".d.ts"):
                continue
            relative = os.path.relpath(os.path.join(directory, name), root)
            stem = os.path.splitext(relative)[0].replace(os.sep, ".")
            if language == "python":
                stem = stem[: -len(".__init__")] if stem.endswith(".__init__") else stem
            modules.add(stem)
    return modules


def evaluate(case: Case, analyze) -> Outcome:
    outcome = Outcome(case)
    try:
        built = call_graph(analyze(case.root), case.root, case.suite.split("/")[0])
    except ResolverUnavailable:
        raise
    except Exception as error:  # a crash is a failed case, and says so
        outcome.error = repr(error)
        return outcome

    modules = _program_modules(case.root, case.suite.split("/")[0]) if case.suite.split("/")[0] in _SOURCE_SUFFIXES else set()

    language = case.suite.split("/")[0]
    keys = set(case.expected)

    def internal(name: str) -> bool:
        if name.startswith("<"):
            return False
        if language in _SOURCE_SUFFIXES:
            return any(name == m or name.startswith(m + ".") for m in modules)
        # Elsewhere a callee belongs to the program when the ground truth lists it
        # as a function of its own: every function the program defines is a key.
        return name in keys

    for caller, callees in case.expected.items():
        for callee in callees:
            if internal(callee):
                outcome.expected_edges += 1
                if callee in built.internal.get(caller, ()):
                    outcome.found_edges += 1
                else:
                    outcome.missing.append((caller, callee))
            else:
                outcome.external_expected += 1
                last = callee.split(".")[-1]
                if any(text and re.split(r"\.|::|->", text)[-1] == last for text in built.external.get(caller, ())):
                    outcome.external_found += 1
    for caller, callees in built.internal.items():
        for callee in callees:
            outcome.produced_edges += 1
            if callee in case.expected.get(caller, ()):
                outcome.correct_edges += 1
            elif caller in case.expected:
                outcome.extra.append((caller, callee))
    outcome.to_value = sum(len(v) for v in built.to_value.values())
    outcome.sound = not outcome.missing
    outcome.complete = not outcome.extra
    return outcome


def score(suite_dir: Path, analyze) -> tuple[Score, list[Outcome]]:
    cases = discover(suite_dir)
    total = Score(f"{suite_dir.parent.name}/{suite_dir.name}")
    outcomes = []
    for case in cases:
        outcome = evaluate(case, analyze)
        outcomes.append(outcome)
        total.cases += 1
        total.sound += outcome.sound
        total.complete += outcome.complete
        total.expected_edges += outcome.expected_edges
        total.found_edges += outcome.found_edges
        total.produced_edges += outcome.produced_edges
        total.correct_edges += outcome.correct_edges
        total.to_value += outcome.to_value
        total.external_expected += outcome.external_expected
        total.external_found += outcome.external_found
        total.errors += outcome.error is not None
        row = total.by_category.setdefault(case.category, [0, 0, 0])
        row[0] += 1
        row[1] += outcome.sound
        row[2] += outcome.complete
    return total, outcomes


class ResolverUnavailable(Exception):
    """The toolchain a language's resolver needs is not installed here."""


def adapter_for(suite_name: str):
    """The analyzer a suite runs, with the resolver on - that is what is measured."""
    language = suite_name.split("/")[0]
    adapters = {
        "python": callix.PythonAdapter,
        "typescript": callix.TypeScriptAdapter,
        "go": callix.GoAdapter,
        "rust": callix.RustAdapter,
        "php": callix.PhpAdapter,
        "c": callix.CAdapter,
        "cpp": callix.CppAdapter,
    }
    factory = adapters.get(language)
    if factory is None:
        raise KeyError(f"no adapter registered for the {language!r} suites")

    def analyze(root):
        graph = factory().analyze(str(root))
        if graph.metadata.get("resolver_status") == "unavailable":
            raise ResolverUnavailable(language)
        return graph

    return analyze
