#!/usr/bin/env python3
"""
Golden-file tests over the fixture projects in `tests/fixtures/`.

Two levels, for two different failure modes:

    structure   The whole graph, serialized, compared byte for byte against
                `tests/golden/<lang>.structure.json`. Resolution is off, so
                this is a pure function of the source tree and needs no
                language toolchain — it runs anywhere.

    resolved    Counts per node and relation kind plus the resolver metrics,
                compared against `tests/golden/resolved.summary.json`. Full
                graphs would be too brittle to freeze, but a kind that stops
                being emitted, or a resolved share that drops, shows up here.
                Skipped per language when its toolchain is missing.

The golden files were cross-validated against graphlens — the implementation
callix replaces — while that project still ran; all four fixtures matched it
byte for byte. Regenerate with `--update` only when a change is understood and
intended.
"""

from __future__ import annotations

import argparse
import collections
import json
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"
GOLDEN = HERE / "golden"
SUMMARY = GOLDEN / "resolved.summary.json"

#: Language → the executable its resolver needs, if any.
TOOLCHAIN = {"go": "go", "rust": "rust-analyzer"}


def adapters() -> dict:
    """Import lazily so a bad build fails as a test, not as a stack trace."""
    import callix

    return {
        "python": callix.PythonAdapter,
        "typescript": callix.TypeScriptAdapter,
        "go": callix.GoAdapter,
        "rust": callix.RustAdapter,
    }


def structure(lang: str, cls) -> dict:
    graph = cls(resolve=False).analyze(FIXTURES / lang)
    # Metrics differ from graphlens by design (callix skips the pass entirely)
    # and carry a timing, so they are not part of what is frozen.
    data = graph.to_dict()
    data["metadata"].pop("resolver_metrics", None)
    return data


def summary(lang: str, cls) -> dict:
    graph = cls().analyze(FIXTURES / lang)
    metrics = graph.metadata["resolver_metrics"]
    return {
        "resolver_status": graph.metadata["resolver_status"],
        "nodes": dict(
            sorted(
                collections.Counter(
                    n.kind.value for n in graph.nodes.values()
                ).items()
            )
        ),
        "relations": dict(
            sorted(
                collections.Counter(r.kind.value for r in graph.relations).items()
            )
        ),
        # Seconds are excluded: they are a timing, not a result.
        "resolver": {
            k: metrics[k]
            for k in ("queries", "resolved", "internal", "external", "unresolved")
        },
    }


def _canonical(data: dict) -> str:
    return json.dumps(data, indent=2, sort_keys=True, ensure_ascii=False) + "\n"


def _report(label: str, expected: str, actual: str) -> bool:
    if expected == actual:
        print(f"  ok       {label}")
        return True
    print(f"  FAILED   {label}")
    import difflib

    diff = difflib.unified_diff(
        expected.splitlines(), actual.splitlines(),
        fromfile="golden", tofile="actual", lineterm="", n=2,
    )
    for line in list(diff)[:40]:
        print(f"           {line}")
    return False


def run(update: bool) -> int:
    langs = adapters()
    ok = True

    print("structure (no resolver, no toolchain needed)")
    for lang, cls in langs.items():
        path = GOLDEN / f"{lang}.structure.json"
        actual = _canonical(structure(lang, cls))
        if update:
            path.write_text(actual, encoding="utf-8")
            print(f"  updated  {lang}")
            continue
        ok &= _report(lang, path.read_text(encoding="utf-8"), actual)

    print("\nresolved (kind counts and resolver metrics)")
    stored = json.loads(SUMMARY.read_text(encoding="utf-8")) if SUMMARY.exists() else {}
    fresh = dict(stored)
    for lang, cls in langs.items():
        needs = TOOLCHAIN.get(lang)
        if needs and shutil.which(needs) is None:
            print(f"  skipped  {lang} ({needs} not on PATH)")
            continue
        result = summary(lang, cls)
        if update:
            fresh[lang] = result
            print(f"  updated  {lang}")
            continue
        ok &= _report(lang, _canonical(stored.get(lang, {})), _canonical(result))

    if update:
        SUMMARY.write_text(_canonical(fresh), encoding="utf-8")

    if update:
        print("\ngolden files rewritten")
        return 0
    print("\nall good" if ok else "\nmismatches above")
    return 0 if ok else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--update", action="store_true", help="rewrite the golden files"
    )
    return run(parser.parse_args().update)


if __name__ == "__main__":
    sys.exit(main())
