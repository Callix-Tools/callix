"""Prints how accurate each suite is, and keeps the floor in thresholds.json.

    python tests/accuracy/run.py                      # every suite
    python tests/accuracy/run.py python/pycg -v       # one suite, with every miss
    python tests/accuracy/run.py --update             # raise the floor, never lower it

The floor is a ratchet: a change that makes a suite worse fails the test, a change
that makes it better is recorded by `--update` and cannot be undone silently.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import harness  # noqa: E402

THRESHOLDS = harness.ROOT / "thresholds.json"
HIGHER_IS_BETTER = ("sound", "complete", "recall", "precision")


def load_thresholds() -> dict[str, dict[str, float]]:
    return json.loads(THRESHOLDS.read_text()) if THRESHOLDS.exists() else {}


def suite_name(path: Path) -> str:
    return f"{path.parent.name}/{path.name}"


def run_suite(path: Path) -> tuple[harness.Score, list[harness.Outcome]]:
    return harness.score(path, harness.adapter_for(suite_name(path)))


def report(score: harness.Score, outcomes: list[harness.Outcome], verbose: bool) -> None:
    print(f"== {score.suite}: {score.cases} cases")
    print(f"{'category':<16}{'cases':>6}{'sound':>8}{'complete':>10}")
    for category, (cases, sound, complete) in score.by_category.items():
        print(f"{category:<16}{cases:>6}{sound:>8}{complete:>10}")
    print(f"{'total':<16}{score.cases:>6}{score.sound:>8}{score.complete:>10}")
    print(
        f"edges to a definition of the program: recall {score.recall:.3f} "
        f"({score.found_edges}/{score.expected_edges}), precision {score.precision:.3f} "
        f"({score.correct_edges}/{score.produced_edges})"
    )
    print(
        f"calls that landed on a variable or a parameter instead of a callee: {score.to_value}"
        f" (not scored as wrong, and not counted as found)"
    )
    if score.external_expected:
        print(
            f"callees outside the program: {score.external_found}/{score.external_expected} "
            f"matched by last name segment (lenient; the graph does not name them)"
        )
    if score.errors:
        print(f"cases that crashed: {score.errors}")
    if verbose:
        for outcome in outcomes:
            if outcome.sound and outcome.complete and not outcome.error:
                continue
            case = outcome.case
            print(f"  {case.category}/{case.name}: sound={outcome.sound} complete={outcome.complete}")
            if outcome.error:
                print(f"      error {outcome.error}")
            for caller, callee in outcome.missing:
                print(f"      missing {caller} -> {callee}")
            for caller, callee in outcome.extra:
                print(f"      extra   {caller} -> {callee}")


def main(argv: list[str]) -> int:
    update = "--update" in argv
    verbose = "-v" in argv
    wanted = [a for a in argv if not a.startswith("-")]
    paths = [p for p in harness.suites() if not wanted or suite_name(p) in wanted]
    thresholds = load_thresholds()
    failed = False
    for path in paths:
        score, outcomes = run_suite(path)
        report(score, outcomes, verbose)
        floor = thresholds.setdefault(score.suite, {})
        measured = score.metrics()
        for key in HIGHER_IS_BETTER:
            if update:
                floor[key] = max(floor.get(key, 0), measured[key])
            elif key in floor and measured[key] < floor[key]:
                print(f"  REGRESSION {key}: {measured[key]} < {floor[key]}")
                failed = True
        print()
    if update:
        THRESHOLDS.write_text(json.dumps(thresholds, indent=2, sort_keys=True) + "\n")
        print(f"wrote {THRESHOLDS.relative_to(harness.ROOT.parent.parent)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
