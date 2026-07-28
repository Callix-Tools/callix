#!/usr/bin/env python3
"""
callix load benchmark: clones real-world projects and measures analysis.

Three sub-commands, all stdlib-only plus ``callix`` itself:

    analyze   Clone one project at its pinned ref, analyse it, and write a
              single JSON file. Run once per project so each run gets its own
              baseline for the peak-memory reading.

    render    Aggregate the JSON results into a Markdown table and splice it
              into a marked block in README.md. Pure post-processing — no
              adapters needed.

    list      Print the manifest's project names, one per line, for a shell
              loop in CI.

The benchmark only **reports**: a project that fails to clone or analyse
becomes an error row and never aborts the run.
"""

from __future__ import annotations

import argparse
import json
import platform
import resource
import shlex
import shutil
import subprocess
import sys
import time
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_MANIFEST = HERE / "projects.json"
DEFAULT_RESULTS = HERE / "results"
START_MARKER = "<!-- BENCH:START -->"
END_MARKER = "<!-- BENCH:END -->"
# git is resolved to a full path once.
_GIT = shutil.which("git") or "git"

# Directories never worth walking when counting lines of code.
_SKIP_DIRS = {
    ".git",
    "node_modules",
    "vendor",
    "dist",
    "build",
    "target",
    "__pycache__",
    ".venv",
    ".mypy_cache",
    ".pytest_cache",
}


def _adapter_for(lang: str):
    """
    The adapter class for a language name.

    The import is lazy: the ``list`` and ``render`` sub-commands need no built
    module at all and must work on a bare machine.
    """
    import callix  # noqa: PLC0415

    adapters = {
        "python": callix.PythonAdapter,
        "typescript": callix.TypeScriptAdapter,
        "go": callix.GoAdapter,
        "rust": callix.RustAdapter,
    }
    if lang not in adapters:
        msg = f"no adapter for language {lang!r}"
        raise KeyError(msg)
    return adapters[lang]


# ---------------------------------------------------------------------------
# Result model
# ---------------------------------------------------------------------------


@dataclass
class BenchResult:
    """Metrics for a single analysed project."""

    name: str
    langs: list[str]
    description: str = ""
    requested_ref: str = ""
    commit: str = ""
    loc: int = 0
    files: int = 0
    nodes: int = 0
    relations: int = 0
    seconds: float = 0.0
    peak_mem_mb: float = 0.0
    mem_source: str = ""
    resolver_status: str = ""
    resolver_queries: int = 0
    resolver_resolved: int = 0
    resolver_seconds: float = 0.0
    status: str = "ok"
    error: str = ""
    notes: list[str] = field(default_factory=list)

    @property
    def kloc_per_s(self) -> float:
        """Thousands of lines per second — the table's headline number."""
        if self.seconds <= 0:
            return 0.0
        return (self.loc / 1000.0) / self.seconds

    @property
    def resolved_pct(self) -> float:
        """Share of resolver queries that returned a definition, in percent."""
        if self.resolver_queries <= 0:
            return 0.0
        return 100.0 * self.resolver_resolved / self.resolver_queries


# ---------------------------------------------------------------------------
# Measurement
# ---------------------------------------------------------------------------


def peak_memory() -> tuple[float, str]:
    """
    Return ``(peak megabytes, source)`` for the whole process tree.

    The cgroup high-water mark is preferred: it covers the subprocesses the
    resolver spawns (``rust-analyzer scip`` for Rust), which ``getrusage`` for
    the parent alone would miss. The fallback is gentle — a measurement never
    breaks the run.
    """
    for path in ("/sys/fs/cgroup/memory.peak",):  # cgroup v2
        try:
            value = int(Path(path).read_text().strip())
        except (OSError, ValueError):
            continue
        return value / (1024 * 1024), "cgroup.v2"
    for path in ("/sys/fs/cgroup/memory/memory.max_usage_in_bytes",):  # v1
        try:
            value = int(Path(path).read_text().strip())
        except (OSError, ValueError):
            continue
        return value / (1024 * 1024), "cgroup.v1"
    # ru_maxrss is in kilobytes on Linux, bytes on macOS.
    maxrss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    factor = 1024 if sys.platform != "darwin" else 1
    return (maxrss * factor) / (1024 * 1024), "getrusage"


def git_clone(repo: str, ref: str, dest: Path) -> tuple[str, list[str]]:
    """
    Shallow-clone *repo* at *ref* into *dest*; return ``(sha, notes)``.

    A pinned shallow clone is tried first (it works for both tags and
    branches). If the ref is gone, the default branch is used instead — so a
    stale manifest entry still yields a real run, reproducible by SHA, rather
    than an empty row.
    """
    notes: list[str] = []
    if dest.exists():
        shutil.rmtree(dest)
    base = [_GIT, "clone", "--depth", "1", "--quiet"]
    pinned = subprocess.run(
        [*base, "--branch", ref, repo, str(dest)],
        capture_output=True,
        text=True,
        check=False,
    )
    if pinned.returncode != 0:
        notes.append(f"ref '{ref}' unavailable; used the default branch")
        subprocess.run(
            [*base, repo, str(dest)],
            capture_output=True,
            text=True,
            check=True,
        )
    head = subprocess.run(
        [_GIT, "-C", str(dest), "rev-parse", "--short", "HEAD"],
        capture_output=True,
        text=True,
        check=False,
    )
    return head.stdout.strip(), notes


def run_prepare(
    command: object, dest: Path, timeout: float = 600.0
) -> list[str]:
    """
    Run a project's setup command inside *dest*.

    A manifest entry may carry a ``prepare`` command (a string or an argv
    list) that runs after the clone and before analysis. Best-effort by
    design: a missing tool, a non-zero exit, or a timeout only adds a note —
    the resolver degrades rather than the whole run failing.
    """
    argv = command if isinstance(command, list) else shlex.split(str(command))
    argv = [str(a) for a in argv]
    if not argv:
        return []
    label = " ".join(argv)
    try:
        proc = subprocess.run(
            argv,
            cwd=str(dest),
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return [f"prepare timed out ({label})"]
    except (OSError, subprocess.SubprocessError) as exc:
        return [f"prepare failed ({label}): {exc}"]
    if proc.returncode != 0:
        return [f"prepare exited {proc.returncode} ({label})"]
    return []


def count_loc(files: list[Path]) -> tuple[int, int]:
    """Return ``(total_lines, file_count)`` for the files that exist."""
    total = 0
    counted = 0
    for fp in files:
        try:
            with fp.open("rb") as fh:
                total += sum(1 for _ in fh)
            counted += 1
        except OSError:
            continue
    return total, counted


# ---------------------------------------------------------------------------
# analyze sub-command
# ---------------------------------------------------------------------------


def analyze_project(project: dict, workdir: Path) -> BenchResult:
    """Clone and analyse one project, returning its metrics."""
    from callix import (  # noqa: PLC0415 — lazy so list/render need no module
        RESOLVER_METRICS_KEY,
        RESOLVER_STATUS_KEY,
    )

    name = str(project["name"])
    langs = [str(x) for x in project.get("langs", [])]
    result = BenchResult(
        name=name,
        langs=langs,
        description=str(project.get("description", "")),
        requested_ref=str(project.get("ref", "")),
    )

    dest = workdir / name.replace("/", "__")
    try:
        commit, notes = git_clone(
            str(project["repo"]), str(project.get("ref", "")), dest
        )
        result.commit = commit
        result.notes.extend(notes)
    except (subprocess.CalledProcessError, OSError) as exc:
        result.status = "error"
        result.error = f"clone failed: {exc}"
        return result

    prepare = project.get("prepare")
    if prepare:
        result.notes.extend(run_prepare(prepare, dest))

    all_files: set[Path] = set()
    nodes = relations = 0
    statuses: list[str] = []
    res_queries = res_resolved = 0
    res_seconds = 0.0
    start = time.perf_counter()
    try:
        for lang in langs:
            adapter = _adapter_for(lang)()
            all_files.update(adapter.collect_files(dest))
            graph = adapter.analyze(dest)
            nodes += len(graph.nodes)
            relations += len(graph.relations)
            statuses.append(
                str(graph.metadata.get(RESOLVER_STATUS_KEY, "unknown"))
            )
            rm = graph.metadata.get(RESOLVER_METRICS_KEY) or {}
            res_queries += int(rm.get("queries", 0))
            res_resolved += int(rm.get("resolved", 0))
            res_seconds += float(rm.get("seconds", 0.0))
    except Exception as exc:  # noqa: BLE001 — an error row instead of a crash
        result.status = "error"
        result.error = f"{type(exc).__name__}: {exc}"
        result.seconds = time.perf_counter() - start
        return result
    result.seconds = time.perf_counter() - start

    loc, file_count = count_loc(sorted(all_files))
    result.loc = loc
    result.files = file_count
    result.nodes = nodes
    result.relations = relations
    result.resolver_status = _worst_status(statuses)
    result.resolver_queries = res_queries
    result.resolver_resolved = res_resolved
    result.resolver_seconds = res_seconds
    result.peak_mem_mb, result.mem_source = peak_memory()
    return result


def _worst_status(statuses: list[str]) -> str:
    """Collapse the per-language resolver statuses into the worst one."""
    order = {"ok": 0, "degraded": 1, "unavailable": 2}
    if not statuses:
        return "unknown"
    return max(statuses, key=lambda s: order.get(s, 3))


# ---------------------------------------------------------------------------
# render sub-command
# ---------------------------------------------------------------------------


def _fmt_int(value: int) -> str:
    return f"{value:,}".replace(",", " ")


def render_table(results: list[BenchResult], version: str) -> str:
    """Build the Markdown benchmark block from *results*."""
    stamp = datetime.now(UTC).strftime("%Y-%m-%d %H:%M UTC")
    runner = f"{platform.system()} {platform.machine()}"
    mem_src = next(
        (r.mem_source for r in results if r.mem_source), "getrusage"
    )

    lines = [
        START_MARKER,
        "",
        (
            f"_Last run: **{stamp}** · callix `{version}` · "
            f"runner `{runner}` · single cold run, indicative only._"
        ),
        "",
        (
            "| Project | Lang | Commit | LOC | Files | Nodes | Relations "
            "| Time | Peak RSS | KLOC/s | Resolver | Resolved |"
        ),
        "|---|---|---|--:|--:|--:|--:|--:|--:|--:|:--|--:|",
    ]
    for r in results:
        if r.status != "ok":
            lines.append(
                f"| [{r.name}](https://github.com/{r.name}) "
                f"| {', '.join(r.langs)} | `{r.commit or '—'}` "
                f"| — | — | — | — | — | — | — | ⚠️ {r.status} | — |"
            )
            continue
        if r.resolver_queries > 0:
            resolved = (
                f"{r.resolved_pct:.0f}% of {_fmt_int(r.resolver_queries)} "
                f"({r.resolver_seconds:.0f}s)"
            )
        else:
            resolved = "—"
        lines.append(
            f"| [{r.name}](https://github.com/{r.name}) "
            f"| {', '.join(r.langs)} "
            f"| `{r.commit or '—'}` "
            f"| {_fmt_int(r.loc)} "
            f"| {_fmt_int(r.files)} "
            f"| {_fmt_int(r.nodes)} "
            f"| {_fmt_int(r.relations)} "
            f"| {r.seconds:.1f}s "
            f"| {r.peak_mem_mb:,.0f} MB "
            f"| {r.kloc_per_s:.1f} "
            f"| {r.resolver_status} "
            f"| {resolved} |"
        )

    ok = [r for r in results if r.status == "ok"]
    totals_loc = sum(r.loc for r in ok)
    totals_nodes = sum(r.nodes for r in ok)
    totals_time = sum(r.seconds for r in ok)
    totals_q = sum(r.resolver_queries for r in ok)
    totals_r = sum(r.resolver_resolved for r in ok)
    if totals_time > 0:
        total_resolved = (
            f"**{100.0 * totals_r / totals_q:.0f}% of {_fmt_int(totals_q)}**"
            if totals_q > 0
            else ""
        )
        lines.append(
            f"| **Total** | | | **{_fmt_int(totals_loc)}** | | "
            f"**{_fmt_int(totals_nodes)}** | | **{totals_time:.1f}s** | | "
            f"**{(totals_loc / 1000.0) / totals_time:.1f}** | | "
            f"{total_resolved} |"
        )

    notes = sorted({n for r in results for n in r.notes})
    if notes:
        lines.extend(["", *(f"> ℹ️ {n}" for n in notes)])

    lines.extend(
        [
            "",
            (
                f"<sub>Peak RSS measured via `{mem_src}` (whole process tree, "
                "including the rust-analyzer subprocess). "
                "KLOC/s = analysed thousands-of-lines per second. "
                "Generated by "
                "[`benchmarks/run_benchmarks.py`](benchmarks/run_benchmarks.py)."
                "</sub>"
            ),
            "",
            END_MARKER,
        ]
    )
    return "\n".join(lines)


def inject_block(readme: Path, block: str) -> bool:
    """Replace the marked block in *readme*; return whether it changed."""
    text = readme.read_text(encoding="utf-8")
    if START_MARKER in text and END_MARKER in text:
        head, _, rest = text.partition(START_MARKER)
        _, _, tail = rest.partition(END_MARKER)
        new = f"{head}{block}{tail}"
    else:  # first run — append a fresh section
        section = f"\n\n## Benchmarks\n\n{block}\n"
        new = text.rstrip() + section + "\n"
    if new == text:
        return False
    readme.write_text(new, encoding="utf-8")
    return True


def load_results(results_dir: Path) -> list[BenchResult]:
    """Load every ``*.json`` result file, ordered by name."""
    out: list[BenchResult] = []
    for fp in sorted(results_dir.glob("*.json")):
        try:
            data = json.loads(fp.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        known = set(BenchResult.__dataclass_fields__)
        out.append(
            BenchResult(**{k: v for k, v in data.items() if k in known})
        )
    return out


def order_by_manifest(
    results: list[BenchResult], manifest_path: Path
) -> list[BenchResult]:
    """Order *results* to match the manifest; unknown names sort last."""
    try:
        order = {
            str(p.get("name")): i
            for i, p in enumerate(load_manifest(manifest_path))
        }
    except (OSError, json.JSONDecodeError):
        return sorted(results, key=lambda r: r.name)
    return sorted(
        results, key=lambda r: (order.get(r.name, len(order)), r.name)
    )


# ---------------------------------------------------------------------------
# Manifest
# ---------------------------------------------------------------------------


def load_manifest(path: Path) -> list[dict]:
    """Return the project list from the JSON manifest."""
    data = json.loads(path.read_text(encoding="utf-8"))
    projects = data.get("projects", []) if isinstance(data, dict) else data
    return [p for p in projects if isinstance(p, dict)]


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _cmd_analyze(args: argparse.Namespace) -> int:
    manifest = load_manifest(Path(args.manifest))
    project = next(
        (p for p in manifest if p.get("name") == args.project), None
    )
    if project is None:
        sys.stderr.write(f"unknown project: {args.project}\n")
        return 2
    workdir = Path(args.workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    result = analyze_project(project, workdir)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    # ensure_ascii=False keeps non-ASCII descriptions readable instead of
    # turning the committed results into \uXXXX escapes.
    out.write_text(
        json.dumps(asdict(result), indent=2, ensure_ascii=False),
        encoding="utf-8",
    )
    sys.stderr.write(
        f"[{result.status}] {result.name}: "
        f"{result.nodes} nodes, {result.relations} relations, "
        f"{result.seconds:.1f}s, {result.peak_mem_mb:.0f}MB"
        + (f"  ({result.error})" if result.error else "")
        + "\n"
    )
    return 0


def _cmd_render(args: argparse.Namespace) -> int:
    results = load_results(Path(args.results))
    if not results:
        sys.stderr.write("no result files found; nothing to render\n")
        return 1
    results = order_by_manifest(results, Path(args.manifest))
    block = render_table(results, args.version)
    if args.print:
        sys.stdout.write(block + "\n")
        return 0
    changed = inject_block(Path(args.readme), block)
    sys.stderr.write(
        ("updated " if changed else "unchanged ") + str(args.readme) + "\n"
    )
    return 0


def _cmd_list(args: argparse.Namespace) -> int:
    for project in load_manifest(Path(args.manifest)):
        name = project.get("name")
        if not name:
            continue
        if args.lang and args.lang not in project.get("langs", []):
            continue
        sys.stdout.write(f"{name}\n")
    return 0


def main(argv: list[str] | None = None) -> int:
    """Entry point for the benchmark CLI."""
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_an = sub.add_parser("analyze", help="analyse one project")
    p_an.add_argument("--project", required=True)
    p_an.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    p_an.add_argument("--workdir", default="/tmp/callix-bench")  # noqa: S108
    p_an.add_argument("--out", required=True)
    p_an.set_defaults(func=_cmd_analyze)

    p_rn = sub.add_parser("render", help="render results into the README")
    p_rn.add_argument("--results", default=str(DEFAULT_RESULTS))
    p_rn.add_argument("--readme", default="README.md")
    p_rn.add_argument("--version", default="dev")
    p_rn.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    p_rn.add_argument("--print", action="store_true")
    p_rn.set_defaults(func=_cmd_render)

    p_ls = sub.add_parser("list", help="print project names")
    p_ls.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    p_ls.add_argument(
        "--lang", default="", help="only projects covering this language"
    )
    p_ls.set_defaults(func=_cmd_list)

    args = parser.parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
