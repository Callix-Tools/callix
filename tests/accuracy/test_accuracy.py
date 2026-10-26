"""The accuracy floor: no suite may score below what it scored when last recorded.

Run `python tests/accuracy/run.py --update` after a change that improves a score,
and commit thresholds.json with it.
"""

from __future__ import annotations

import pytest

import harness
import run


@pytest.mark.parametrize("path", harness.suites(), ids=run.suite_name)
def test_suite_does_not_regress(path) -> None:
    score, outcomes = run.run_suite(path)
    crashed = [o.case.name for o in outcomes if o.error]
    assert not crashed, f"cases that crashed: {crashed}"

    floor = run.load_thresholds().get(score.suite)
    assert floor, f"{score.suite} has no floor in thresholds.json; run tests/accuracy/run.py --update"
    measured = score.metrics()
    worse = {k: (measured[k], floor[k]) for k in run.HIGHER_IS_BETTER if k in floor and measured[k] < floor[k]}
    assert not worse, f"{score.suite} got worse (measured, floor): {worse}"
