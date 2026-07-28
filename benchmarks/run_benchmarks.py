#!/usr/bin/env python3
"""
Нагрузочный бенчмарк callix: клонирует реальные проекты и меряет анализ.

Три подкоманды, все — только stdlib плюс сам ``callix``:

    analyze   Клонирует один проект по закреплённой ссылке, анализирует его
              и пишет один JSON. Запускается по проекту за раз, чтобы у
              каждого прогона была своя точка отсчёта пиковой памяти.

    render    Сворачивает JSON-результаты в Markdown-таблицу и вставляет её
              в размеченный блок README.md. Чистая постобработка — адаптеры
              для неё не нужны.

    list      Печатает имена проектов из манифеста по одному на строку —
              для цикла в CI.

Бенчмарк только **сообщает**: проект, который не склонировался или упал на
анализе, становится строкой с ошибкой и не роняет весь прогон.
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
# git резолвится в полный путь один раз.
_GIT = shutil.which("git") or "git"

# Каталоги, по которым не имеет смысла ходить при подсчёте строк.
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
    Класс адаптера по имени языка.

    Импорт ленивый: подкомандам ``list`` и ``render`` собранный модуль не
    нужен вовсе, и они должны работать на голой машине.
    """
    import callix  # noqa: PLC0415

    adapters = {
        "python": callix.PythonAdapter,
        "typescript": callix.TypeScriptAdapter,
        "go": callix.GoAdapter,
        "rust": callix.RustAdapter,
    }
    if lang not in adapters:
        msg = f"нет адаптера для языка {lang!r}"
        raise KeyError(msg)
    return adapters[lang]


# ---------------------------------------------------------------------------
# Модель результата
# ---------------------------------------------------------------------------


@dataclass
class BenchResult:
    """Метрики одного проанализированного проекта."""

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
        """Тысяч строк в секунду — главная цифра таблицы."""
        if self.seconds <= 0:
            return 0.0
        return (self.loc / 1000.0) / self.seconds

    @property
    def resolved_pct(self) -> float:
        """Доля запросов к резолверу, вернувших определение, в процентах."""
        if self.resolver_queries <= 0:
            return 0.0
        return 100.0 * self.resolver_resolved / self.resolver_queries


# ---------------------------------------------------------------------------
# Измерения
# ---------------------------------------------------------------------------


def peak_memory() -> tuple[float, str]:
    """
    Возвращает ``(пик в мегабайтах, источник)`` для всего дерева процессов.

    Сначала пробуется отметка cgroup: она покрывает и подпроцессы, которые
    поднимает резолвер (у Rust это ``rust-analyzer scip``), а ``getrusage``
    родителя их бы не увидел. Откат мягкий — измерение никогда не роняет
    прогон.
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
    # ru_maxrss на Linux в килобайтах, на macOS в байтах.
    maxrss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    factor = 1024 if sys.platform != "darwin" else 1
    return (maxrss * factor) / (1024 * 1024), "getrusage"


def git_clone(repo: str, ref: str, dest: Path) -> tuple[str, list[str]]:
    """
    Клонирует *repo* на *ref* в *dest*; возвращает ``(SHA, заметки)``.

    Сначала пробуется поверхностный клон по ссылке (работает и для тегов, и
    для веток). Если ссылки больше нет, берётся ветка по умолчанию — тогда
    устаревшая запись манифеста всё равно даёт настоящий прогон,
    воспроизводимый по SHA, а не пустую строку.
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
        notes.append(f"ссылка '{ref}' недоступна, взята ветка по умолчанию")
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
    Выполняет подготовительную команду проекта внутри *dest*.

    Запись манифеста может нести ``prepare`` (строку или список argv),
    который отрабатывает после клона и до анализа. Всё делается
    по-хорошему: отсутствие инструмента, ненулевой код или таймаут только
    добавляют заметку — деградирует резолвер, а не весь прогон.
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
        return [f"prepare не уложился в таймаут ({label})"]
    except (OSError, subprocess.SubprocessError) as exc:
        return [f"prepare не выполнился ({label}): {exc}"]
    if proc.returncode != 0:
        return [f"prepare вышел с кодом {proc.returncode} ({label})"]
    return []


def count_loc(files: list[Path]) -> tuple[int, int]:
    """Возвращает ``(всего строк, файлов)`` по существующим файлам."""
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
# Подкоманда analyze
# ---------------------------------------------------------------------------


def analyze_project(project: dict, workdir: Path) -> BenchResult:
    """Клонирует и анализирует один проект, возвращая его метрики."""
    from callix import (  # noqa: PLC0415 — лениво, чтобы list/render шли без модуля
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
        result.error = f"клон не удался: {exc}"
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
    except Exception as exc:  # noqa: BLE001 — строка с ошибкой вместо падения
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
    """Сводит статусы резолверов по языкам к худшему."""
    order = {"ok": 0, "degraded": 1, "unavailable": 2}
    if not statuses:
        return "unknown"
    return max(statuses, key=lambda s: order.get(s, 3))


# ---------------------------------------------------------------------------
# Подкоманда render
# ---------------------------------------------------------------------------


def _fmt_int(value: int) -> str:
    return f"{value:,}".replace(",", " ")


def render_table(results: list[BenchResult], version: str) -> str:
    """Собирает Markdown-блок бенчмарка из *results*."""
    stamp = datetime.now(UTC).strftime("%Y-%m-%d %H:%M UTC")
    runner = f"{platform.system()} {platform.machine()}"
    mem_src = next(
        (r.mem_source for r in results if r.mem_source), "getrusage"
    )

    lines = [
        START_MARKER,
        "",
        (
            f"_Прогон: **{stamp}** · callix `{version}` · "
            f"машина `{runner}` · один холодный запуск, цифры ориентировочные._"
        ),
        "",
        (
            "| Проект | Язык | Коммит | Строк | Файлов | Узлов | Рёбер "
            "| Время | Пик RSS | KLOC/s | Резолвер | Разрешено |"
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
                f"{r.resolved_pct:.0f}% из {_fmt_int(r.resolver_queries)} "
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
            f"**{100.0 * totals_r / totals_q:.0f}% из {_fmt_int(totals_q)}**"
            if totals_q > 0
            else ""
        )
        lines.append(
            f"| **Всего** | | | **{_fmt_int(totals_loc)}** | | "
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
                f"<sub>Пик RSS снят через `{mem_src}` (всё дерево процессов, "
                "включая подпроцесс rust-analyzer). "
                "KLOC/s — тысяч разобранных строк в секунду. "
                "Собрано "
                "[`benchmarks/run_benchmarks.py`](benchmarks/run_benchmarks.py)."
                "</sub>"
            ),
            "",
            END_MARKER,
        ]
    )
    return "\n".join(lines)


def inject_block(readme: Path, block: str) -> bool:
    """Заменяет размеченный блок в *readme*; возвращает, изменилось ли."""
    text = readme.read_text(encoding="utf-8")
    if START_MARKER in text and END_MARKER in text:
        head, _, rest = text.partition(START_MARKER)
        _, _, tail = rest.partition(END_MARKER)
        new = f"{head}{block}{tail}"
    else:  # первый прогон — дописываем секцию
        section = f"\n\n## Бенчмарки\n\n{block}\n"
        new = text.rstrip() + section + "\n"
    if new == text:
        return False
    readme.write_text(new, encoding="utf-8")
    return True


def load_results(results_dir: Path) -> list[BenchResult]:
    """Читает все ``*.json`` результатов по возрастанию имени."""
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
    """Упорядочивает *results* как в манифесте; неизвестные — в конец."""
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
# Манифест
# ---------------------------------------------------------------------------


def load_manifest(path: Path) -> list[dict]:
    """Возвращает список проектов из JSON-манифеста."""
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
        sys.stderr.write(f"неизвестный проект: {args.project}\n")
        return 2
    workdir = Path(args.workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    result = analyze_project(project, workdir)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(asdict(result), indent=2), encoding="utf-8")
    sys.stderr.write(
        f"[{result.status}] {result.name}: "
        f"{result.nodes} узлов, {result.relations} рёбер, "
        f"{result.seconds:.1f}s, {result.peak_mem_mb:.0f}MB"
        + (f"  ({result.error})" if result.error else "")
        + "\n"
    )
    return 0


def _cmd_render(args: argparse.Namespace) -> int:
    results = load_results(Path(args.results))
    if not results:
        sys.stderr.write("результатов не найдено, рендерить нечего\n")
        return 1
    results = order_by_manifest(results, Path(args.manifest))
    block = render_table(results, args.version)
    if args.print:
        sys.stdout.write(block + "\n")
        return 0
    changed = inject_block(Path(args.readme), block)
    sys.stderr.write(
        ("обновлён " if changed else "без изменений ") + str(args.readme) + "\n"
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
    """Точка входа CLI бенчмарка."""
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_an = sub.add_parser("analyze", help="проанализировать один проект")
    p_an.add_argument("--project", required=True)
    p_an.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    p_an.add_argument("--workdir", default="/tmp/callix-bench")  # noqa: S108
    p_an.add_argument("--out", required=True)
    p_an.set_defaults(func=_cmd_analyze)

    p_rn = sub.add_parser("render", help="вставить результаты в README")
    p_rn.add_argument("--results", default=str(DEFAULT_RESULTS))
    p_rn.add_argument("--readme", default="README.md")
    p_rn.add_argument("--version", default="dev")
    p_rn.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    p_rn.add_argument("--print", action="store_true")
    p_rn.set_defaults(func=_cmd_render)

    p_ls = sub.add_parser("list", help="напечатать имена проектов")
    p_ls.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    p_ls.add_argument(
        "--lang", default="", help="только проекты с этим языком"
    )
    p_ls.set_defaults(func=_cmd_list)

    args = parser.parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
