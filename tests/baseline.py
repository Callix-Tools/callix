#!/usr/bin/env python3
"""
Fingerprints of real projects — the wide half of the regression net.

`tests/golden/` freezes whole graphs, which only works because the fixtures are
tiny. Real repositories cannot be frozen that way: ruff alone serializes to
tens of megabytes. So what is stored here is a *fingerprint* — deterministic,
a few hundred lines per project, and diagnostic enough to point at the file
that changed:

    totals        node and relation counts
    kinds         counts per node kind and per relation kind
    files         per source file, its node count, relation count and a hash
                  over its nodes

Only the structural graph is fingerprinted (`resolve=False`). That keeps the
value a pure function of the source tree — no toolchain, no interpreter
version, no machine — which is what a baseline has to be.

    capture     analyse the pinned checkouts and write tests/baseline/*.json
    compare     re-analyse and report where the fingerprint moved
    crosscheck  same, but against graphlens, recording the divergence

Projects come from `benchmarks/projects.json`; checkouts are expected under
`--workdir`, cloned by the benchmark harness or by `capture --clone`.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
MANIFEST = ROOT / "benchmarks" / "projects.json"
BASELINE = HERE / "baseline"
_GIT = shutil.which("git") or "git"


def projects(only: str | None = None, lang: str | None = None) -> list[dict]:
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))["projects"]
    if only:
        data = [p for p in data if p["name"] == only]
    if lang:
        data = [p for p in data if lang in p["langs"]]
    return data


def slug(name: str) -> str:
    return name.replace("/", "__")


def clone(project: dict, dest: Path) -> None:
    """Shallow-clone the pinned ref; leave an existing checkout alone."""
    if (dest / ".git").exists():
        return
    dest.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [_GIT, "clone", "--depth", "1", "--quiet", "--branch",
         str(project["ref"]), str(project["repo"]), str(dest)],
        check=True,
    )


def fingerprint(graph) -> dict:
    """A deterministic summary of a structural graph."""
    per_file: dict[str, dict] = {}
    for node in graph.nodes.values():
        path = node.file_path
        if path is None:
            continue
        entry = per_file.setdefault(path, {"nodes": 0, "relations": 0, "digest": []})
        entry["nodes"] += 1
        # The span and the qualified name together pin down what the visitor
        # decided; the id alone would hide a moved span.
        span = node.span
        entry["digest"].append(
            f"{node.kind.value}\t{node.qualified_name}\t"
            f"{span.start_line if span else 0}:{span.start_col if span else 0}"
        )

    by_id = {n.id: n for n in graph.nodes.values()}
    for relation in graph.relations:
        source = by_id.get(relation.source_id)
        if source is not None and source.file_path is not None:
            per_file.setdefault(
                source.file_path, {"nodes": 0, "relations": 0, "digest": []}
            )["relations"] += 1

    files = {}
    for path in sorted(per_file):
        entry = per_file[path]
        payload = "\n".join(sorted(entry["digest"])).encode("utf-8")
        files[path] = {
            "nodes": entry["nodes"],
            "relations": entry["relations"],
            "hash": hashlib.sha256(payload).hexdigest()[:16],
        }

    return {
        "totals": {
            "nodes": len(graph.nodes),
            "relations": len(graph.relations),
            "files": len(files),
        },
        "kinds": {
            "nodes": dict(
                sorted(
                    collections.Counter(
                        n.kind.value for n in graph.nodes.values()
                    ).items()
                )
            ),
            "relations": dict(
                sorted(
                    collections.Counter(
                        r.kind.value for r in graph.relations
                    ).items()
                )
            ),
        },
        "files": files,
    }


def analyse_callix(lang: str, root: Path) -> dict:
    import callix

    adapters = {
        "python": callix.PythonAdapter,
        "typescript": callix.TypeScriptAdapter,
        "go": callix.GoAdapter,
        "rust": callix.RustAdapter,
    }
    return fingerprint(adapters[lang](resolve=False).analyze(root))


def analyse_graphlens(lang: str, root: Path, graphlens: Path) -> dict:
    """Run the archived reference implementation in its own interpreter."""
    script = f"""
import json, sys
sys.path.insert(0, {str(graphlens / "src")!r})
for pkg in ("python", "typescript", "go", "rust"):
    sys.path.insert(0, {str(graphlens / "packages")!r} + f"/graphlens-{{pkg}}/src")
sys.path.insert(0, {str(HERE)!r})
from graphlens.contracts import SymbolResolver
from graphlens.status import ResolverStatus

class Off(SymbolResolver):
    def prepare(self, r, f): pass
    def definition_at(self, f, l, c): return None
    def infer_type_at(self, f, l, c): return None
    def references_to(self, f, l, c): return []
    def status(self): return ResolverStatus.UNAVAILABLE

lang = {lang!r}
if lang == "python":
    from graphlens_python import PythonAdapter as Ad
elif lang == "typescript":
    from graphlens_typescript import TypescriptAdapter as Ad
elif lang == "go":
    from graphlens_go import GoAdapter as Ad
else:
    from graphlens_rust import RustAdapter as Ad

from baseline import fingerprint
print(json.dumps(fingerprint(Ad(resolver=Off()).analyze({str(root)!r}))))
"""
    proc = subprocess.run(
        [str(graphlens / ".venv" / "bin" / "python"), "-c", script],
        capture_output=True, text=True, check=True,
    )
    return json.loads(proc.stdout)


def _diff(expected: dict, actual: dict) -> list[str]:
    """Human-readable differences between two fingerprints."""
    notes: list[str] = []
    for key in ("nodes", "relations", "files"):
        want, got = expected["totals"][key], actual["totals"][key]
        if want != got:
            notes.append(f"totals.{key}: {want} -> {got}")
    for group in ("nodes", "relations"):
        want, got = expected["kinds"][group], actual["kinds"][group]
        for kind in sorted(set(want) | set(got)):
            if want.get(kind, 0) != got.get(kind, 0):
                notes.append(
                    f"kinds.{group}.{kind}: {want.get(kind, 0)} -> {got.get(kind, 0)}"
                )
    want_files, got_files = expected["files"], actual["files"]
    missing = sorted(set(want_files) - set(got_files))
    added = sorted(set(got_files) - set(want_files))
    changed = sorted(
        p for p in set(want_files) & set(got_files) if want_files[p] != got_files[p]
    )
    for path in missing[:10]:
        notes.append(f"file gone: {path}")
    for path in added[:10]:
        notes.append(f"file new: {path}")
    for path in changed[:10]:
        notes.append(f"file changed: {path}")
    for label, group in (("gone", missing), ("new", added), ("changed", changed)):
        if len(group) > 10:
            notes.append(f"... and {len(group) - 10} more {label}")
    return notes


def _canonical(data: dict) -> str:
    return json.dumps(data, indent=2, sort_keys=True) + "\n"


def cmd_capture(args: argparse.Namespace) -> int:
    BASELINE.mkdir(parents=True, exist_ok=True)
    workdir = Path(args.workdir)
    for project in projects(args.project, args.lang):
        dest = workdir / slug(project["name"])
        if args.clone:
            clone(project, dest)
        if not dest.exists():
            print(f"  missing  {project['name']} ({dest})")
            continue
        data = analyse_callix(project["langs"][0], dest)
        data["project"] = project["name"]
        data["ref"] = project["ref"]
        (BASELINE / f"{slug(project['name'])}.json").write_text(
            _canonical(data), encoding="utf-8"
        )
        totals = data["totals"]
        print(
            f"  captured {project['name']:<22} "
            f"{totals['nodes']:>7} nodes  {totals['relations']:>7} relations"
        )
    return 0


def cmd_compare(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir)
    ok = True
    for project in projects(args.project, args.lang):
        path = BASELINE / f"{slug(project['name'])}.json"
        dest = workdir / slug(project["name"])
        if not path.exists() or not dest.exists():
            print(f"  skipped  {project['name']}")
            continue
        expected = json.loads(path.read_text(encoding="utf-8"))
        actual = analyse_callix(project["langs"][0], dest)
        notes = _diff(expected, actual)
        if not notes:
            print(f"  ok       {project['name']}")
            continue
        ok = False
        print(f"  MOVED    {project['name']}")
        for note in notes:
            print(f"             {note}")
    print("\nall good" if ok else "\nfingerprints moved above")
    return 0 if ok else 1


def cmd_crosscheck(args: argparse.Namespace) -> int:
    """Compare against graphlens and print a report, never a pass/fail gate."""
    graphlens = Path(args.graphlens)
    workdir = Path(args.workdir)
    if not (graphlens / ".venv" / "bin" / "python").exists():
        print(f"graphlens is not runnable at {graphlens}")
        return 2
    for project in projects(args.project, args.lang):
        dest = workdir / slug(project["name"])
        if not dest.exists():
            print(f"  skipped  {project['name']}")
            continue
        lang = project["langs"][0]
        mine = analyse_callix(lang, dest)
        try:
            theirs = analyse_graphlens(lang, dest, graphlens)
        except subprocess.CalledProcessError as exc:
            print(f"  ERROR    {project['name']}: {exc.stderr.strip()[-200:]}")
            continue
        notes = _diff(theirs, mine)
        if not notes:
            print(f"  identical {project['name']}")
            continue
        print(f"  differs   {project['name']} ({len(notes)} notes)")
        for note in notes[:12]:
            print(f"              {note}")
    return 0


def _common(sub, name: str, help_text: str):
    """Every sub-command takes the same selectors, so order never matters."""
    parser = sub.add_parser(name, help=help_text)
    parser.add_argument(
        "--workdir", default="/tmp/callix-baseline",  # noqa: S108
        help="where the pinned checkouts live",
    )
    parser.add_argument("--project", default="", help="one project by name")
    parser.add_argument("--lang", default="", help="one language")
    return parser


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_cap = _common(sub, "capture", "write the baselines")
    p_cap.add_argument("--clone", action="store_true", help="clone what is missing")
    p_cap.set_defaults(func=cmd_capture)

    _common(sub, "compare", "check the baselines").set_defaults(func=cmd_compare)

    p_cross = _common(sub, "crosscheck", "report divergence from graphlens")
    p_cross.add_argument("--graphlens", default=str(Path.home() / "project/graphlens"))
    p_cross.set_defaults(func=cmd_crosscheck)

    args = parser.parse_args()
    args.project = args.project or None
    args.lang = args.lang or None
    return int(args.func(args))


if __name__ == "__main__":
    sys.exit(main())
