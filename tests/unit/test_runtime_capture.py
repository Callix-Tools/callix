"""The runtime-artifact capture helpers: callgrind parsing and the artifact writer.

Pure Python on synthetic text: no clone, no toolchain, no valgrind.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

CAPTURE = Path(__file__).resolve().parents[1] / "runtime" / "capture"
sys.path.insert(0, str(CAPTURE))

import callgrind  # noqa: E402
import common  # noqa: E402

CALLGRIND = """\
# callgrind format
version: 1
creator: callgrind-3.22.0
positions: line
events: Ir

fl=(1) /proj/src/main.c
fn=(1) main
10 5
+1 3
cfl=(2) /proj/src/util.c
cfn=(2) helper
calls=3 20
+2 40
fi=(3) /proj/src/inl.h
+1 2
fe=(1)
cfn=(3) printf
cfl=(4) /usr/lib/libc.c
calls=1 0
* 9

fl=(2)
fn=(2)
20 1
cfl=(1)
cfn=(1)
calls=2 10
* 5
fn=(5) rec'2
30 1
"""


def _write(tmp_path: Path, text: str = CALLGRIND) -> Path:
    path = tmp_path / "callgrind.out.1"
    path.write_text(text, encoding="utf-8")
    return path


def _artifact(calls):
    return common.build_artifact(
        project="o/r", ref="1.0", sha="abc", language="c", tool="t",
        workload="w", command="c", calls=calls,
    )


def test_callgrind_keeps_only_in_project_calls(tmp_path):
    calls = callgrind.parse([_write(tmp_path)], "/proj")
    assert sorted(calls) == [
        (("src/main.c", "main", 10), ("src/util.c", "helper", 20), 3),
        (("src/util.c", "helper", 20), ("src/main.c", "main", 10), 2),
    ]


def test_callgrind_inlined_lines_do_not_move_the_function(tmp_path):
    # main has cost lines at 10, 11 and (under fi=) 12; only the first two count.
    calls = callgrind.parse([_write(tmp_path)], "/proj")
    assert any(c[0] == ("src/main.c", "main", 10) for c in calls)


def test_callgrind_sums_counts_over_files(tmp_path):
    first = _write(tmp_path)
    second = tmp_path / "callgrind.out.2"
    second.write_text(CALLGRIND, encoding="utf-8")
    calls = callgrind.parse([first, second], "/proj")
    assert {c[2] for c in calls} == {6, 4}


def test_callgrind_uncompressed_names_and_cycles(tmp_path):
    text = (
        "positions: line\nevents: Ir\n"
        "fl=/proj/a.c\nfn=f\n5 1\ncfl=/proj/a.c\ncfn=g\ncalls=2 9\n5 1\n"
        "cfn=<cycle 1>\ncalls=1 9\n5 1\n"
        "fn=g\n9 1\n"
    )
    calls = callgrind.parse([_write(tmp_path, text)], "/proj")
    assert calls == [(("a.c", "f", 5), ("a.c", "g", 9), 2)]


def test_callgrind_instruction_column_uses_last_column_as_line(tmp_path):
    text = (
        "positions: instr line\nevents: Ir\n"
        "fl=/proj/a.c\nfn=f\n0x10 7 1\n+4 +1 1\n"
        "cfn=g\ncalls=1 0x40 20\n* * 1\n"
        "fn=g\n0x40 20 1\n"
    )
    calls = callgrind.parse([_write(tmp_path, text)], "/proj")
    assert calls == [(("a.c", "f", 7), ("a.c", "g", 20), 1)]


def test_clean_name_and_relativize():
    assert common.clean_name("core::foo::h0123456789abcdef") == "core::foo"
    assert common.clean_name("  rec'2 ") == "rec"
    assert common.relativize("/proj/src/a.c", "/proj") == "src/a.c"
    assert common.relativize("/usr/lib/a.c", "/proj") is None
    assert common.relativize("/proj/deps/x/a.c", "/proj") is None
    assert common.relativize("???", "/proj") is None


def test_build_artifact_is_sorted_and_merges_pairs():
    art = _artifact([
        (("b.c", "g", 4), ("a.c", "f", 9), 1),
        (("b.c", "g", 3), ("a.c", "f", 7), 2),
    ])
    assert [f["name"] for f in art["functions"]] == ["f", "g"]
    assert art["functions"][0]["line"] == 7
    assert art["functions"][1]["line"] == 3
    assert art["edges"] == [[1, 0, 3]]
    assert common.validate(art) == []


def test_dump_is_stable_json_and_round_trips(tmp_path):
    art = _artifact([(("a.c", "f", 1), ("a.c", "g", 2), 5)])
    text = common.dump(art)
    assert common.dump(art) == text
    assert json.loads(text) == art
    assert list(json.loads(text)) == list(common.KEY_ORDER)
    out = tmp_path / "o__r.json"
    common.write_artifact(out, art)
    assert common.load_artifact(out) == art


def test_validate_rejects_malformed_artifacts():
    art = _artifact([
        (("a.c", "f", 1), ("a.c", "g", 2), 5),
        (("a.c", "g", 2), ("a.c", "f", 1), 1),
    ])
    bad = json.loads(json.dumps(art))
    bad["edges"] = [[1, 0, 1], [0, 1, 5]]
    assert any("unsorted" in p for p in common.validate(bad))
    bad = json.loads(json.dumps(art))
    bad["edges"][0][1] = 9
    assert any("invalid id" in p for p in common.validate(bad))
    bad = json.loads(json.dumps(art))
    bad["functions"][0]["file"] = "/abs/a.c"
    assert any("bad file" in p for p in common.validate(bad))
    bad = json.loads(json.dumps(art))
    bad["schema"] = 2
    assert common.validate(bad)
    with pytest.raises(ValueError):
        common.write_artifact("/nonexistent/x.json", bad)
