"""Mapping and scoring of runtime artifacts, on a synthetic graph.

No clone, no toolchain, no callix binary: the graph is a dict in the shape
`callix analyze` writes, the artifact a dict in the shape of
`tests/runtime/README.md`.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import pytest

RUNTIME = Path(__file__).resolve().parents[1] / "runtime"
sys.path.insert(0, str(RUNTIME))
sys.path.insert(0, str(RUNTIME / "capture"))

import common  # noqa: E402
import runtime  # noqa: E402


def node(ident, name, file, span=None, kind="function", qn=None, declared=None):
    meta = {"declared_at": declared} if declared else {}
    return {
        "id": ident, "kind": kind, "qualified_name": qn or name, "name": name,
        "file_path": file, "span": span, "metadata": meta,
    }


def external(ident, qn):
    return {
        "id": ident, "kind": "external_symbol", "qualified_name": qn,
        "name": qn.split("@")[-1], "file_path": None, "span": None, "metadata": {},
    }


def graph(nodes, calls):
    relations = [
        {"source_id": s, "target_id": t, "kind": "calls", "metadata": {"resolution": r}}
        for s, t, r in calls
    ]
    return runtime.Graph({"nodes": nodes, "relations": relations})


def artifact(language, functions, edges):
    return {
        "schema": 1, "project": "o/r", "ref": "1", "sha": "abc", "language": language,
        "tool": "t", "workload": "w", "command": "c",
        "functions": [
            {"id": i, "file": f, "name": n, "line": ln}
            for i, (f, n, ln) in enumerate(functions)
        ],
        "edges": [list(e) for e in edges],
    }


def simple():
    return [
        node("a", "a", "x.c", [1, 1, 3, 2]),
        node("b", "b", "x.c", [5, 1, 7, 2]),
        node("c", "c", "x.c", [9, 1, 11, 2]),
    ]


def test_found_counts_every_resolution_including_flow():
    g = graph(simple(), [("a", "b", "definition"), ("a", "c", "flow")])
    art = artifact("c", [("x.c", "a", 1), ("x.c", "b", 5), ("x.c", "c", 9)],
                   [(0, 1, 4), (0, 2, 1)])
    result = runtime.score(art, g)
    assert result["found"] == 2
    assert result["found_by_resolution"] == {"definition": 1, "flow": 1}
    assert result["recall"] == 1.0
    assert result["functions_mapped"] == 3


def test_wrong_target_to_an_interface_method_of_the_same_name():
    nodes = [
        node("main", "main", "m.go", [1, 1, 5, 2], qn="p.main"),
        node("iface", "Run", "i.go", [1, 1, 1, 20], kind="method", qn="p.Runner.Run"),
        node("impl", "Run", "impl.go", [1, 1, 3, 2], kind="method", qn="p.Job.Run"),
    ]
    g = graph(nodes, [("main", "iface", "definition")])
    art = artifact("go", [("m.go", "p.main", 1), ("impl.go", "p.(*Job).Run", 1)], [(0, 1, 2)])
    result = runtime.score(art, g)
    assert (result["wrong_target"], result["found"], result["missing"]) == (1, 0, 0)
    assert result["recall"] == 0.0


def test_wrong_target_to_an_external_symbol():
    nodes = [
        node("f", "f", "a.py", [1, 1, 4, 1], qn="a.f"),
        node("d", "deco", "b.py", [1, 1, 3, 1], qn="b.deco"),
        external("e", "deco@a.py:2:1"),
    ]
    g = graph(nodes, [("f", "e", "external")])
    art = artifact("python", [("a.py", "a.f", 1), ("b.py", "b.deco", 1)], [(0, 1, 1)])
    assert runtime.score(art, g)["wrong_target"] == 1


def test_missing_when_the_caller_never_calls_that_name():
    g = graph(simple(), [("a", "b", "definition")])
    art = artifact("c", [("x.c", "a", 1), ("x.c", "c", 9)], [(0, 1, 1)])
    result = runtime.score(art, g)
    assert (result["missing"], result["wrong_target"], result["found"]) == (1, 0, 0)


def test_unmapped_reasons_and_never_a_miss():
    nodes = simple() + [
        node("d1", "dup", "x.c", [20, 1, 22, 2]),
        node("d2", "dup", "x.c", [30, 1, 32, 2]),
    ]
    g = graph(nodes, [])
    art = artifact("c", [
        ("x.c", "a", 1), ("nowhere.c", "ghost", 1), ("x.c", "absent", 0), ("x.c", "dup", 0),
    ], [(0, 1, 1), (0, 2, 1), (0, 3, 1)])
    result = runtime.score(art, g)
    assert result["unmapped"] == 3
    assert result["missing"] == 0
    assert result["recall"] is None
    reasons = {k: v["count"] for k, v in result["unmapped_functions"].items()}
    assert reasons == {"ambiguous": 1, "no_file": 1, "no_name": 1}


def test_implicit_calls_have_their_own_category():
    nodes = [
        node("m", "main", "m.cpp", [1, 1, 9, 2]),
        node("ctor", "Foo", "m.cpp", [11, 1, 12, 2], kind="method", qn="Foo::Foo"),
    ]
    g = graph(nodes, [("m", "ctor", "definition")])
    art = artifact("cpp", [
        ("m.cpp", "main", 1), ("m.cpp", "Foo::Foo(int)", 11),
        ("m.cpp", "Foo::~Foo()", 20), ("m.cpp", "Foo::operator=(Foo const&)", 30),
    ], [(0, 1, 1), (0, 2, 1), (0, 3, 1)])
    result = runtime.score(art, g)
    assert result["implicit"] == 3
    assert result["implicit_found"] == 1
    assert (result["missing"], result["unmapped"]) == (0, 0)


@pytest.mark.parametrize(("language", "name", "implicit"), [
    ("python", "pkg.Foo.__init__", True),
    ("python", "pkg.Foo.run", False),
    ("rust", "<proj::Foo as core::ops::drop::Drop>::drop", True),
    ("rust", "<proj::Foo as proj::Shape>::area", False),
    ("rust", "<alloc::vec::Vec<T> as core::iter::traits::iterator::Iterator>::next", True),
    ("php", "App\\Foo->__construct", True),
    ("typescript", "Foo.constructor", True),
    ("go", "pkg.init", True),
    ("c", "main", False),
    ("cpp", "ns::Foo::run(int)", False),
])
def test_is_implicit(language, name, implicit):
    assert runtime.is_implicit(name, language) is implicit


def test_go_closure_is_folded_into_its_parent():
    nodes = [
        node("run", "Run", "a.go", [1, 1, 10, 2], qn="p.Run"),
        node("help", "Helper", "a.go", [12, 1, 14, 2], qn="p.Helper"),
    ]
    g = graph(nodes, [("run", "help", "definition")])
    art = artifact("go", [
        ("a.go", "p.Run", 1), ("a.go", "p.Run.func1", 5), ("a.go", "p.Helper", 12),
    ], [(0, 1, 1), (1, 2, 3)])
    result = runtime.score(art, g)
    assert result["folded"] == 1
    assert result["found"] == 1
    assert result["recall"] == 1.0


def test_anonymous_function_maps_by_line_when_the_name_is_unknown():
    nodes = [node("outer", "outer", "a.ts", [1, 1, 20, 2])]
    fn = {"id": 0, "file": "a.ts", "name": "<anonymous>", "line": 7}
    assert runtime.map_function(fn, graph(nodes, []), "typescript") == ("outer", None)


def test_recall_is_rounded_and_weighted_by_calls():
    g = graph(simple(), [("a", "b", "definition")])
    art = artifact("c", [("x.c", "a", 1), ("x.c", "b", 5), ("x.c", "c", 9)],
                   [(0, 1, 3), (0, 2, 1), (1, 2, 1)])
    result = runtime.score(art, g)
    assert (result["found"], result["missing"]) == (1, 2)
    assert result["recall"] == 0.3333
    assert result["recall_by_calls"] == 0.6
    assert result["calls"]["found"] == 3


def test_instances_that_map_to_one_node_count_once():
    nodes = [
        node("m", "main", "m.cpp", [1, 1, 9, 2]),
        node("bar", "bar", "m.cpp", [11, 1, 13, 2], kind="method", qn="Foo::bar"),
    ]
    g = graph(nodes, [("m", "bar", "definition")])
    art = artifact("cpp", [
        ("m.cpp", "main", 1), ("m.cpp", "Foo<int>::bar(int)", 11),
        ("m.cpp", "Foo<char>::bar(char)", 11),
    ], [(0, 1, 2), (0, 2, 3)])
    result = runtime.score(art, g)
    assert result["edges"] == 2
    assert result["pairs"] == 1
    assert result["found"] == 1
    assert result["calls"]["found"] == 5


def test_declared_at_maps_a_header_function():
    nodes = [node("f", "buffer_length", "src/buffer.c", [3, 1, 6, 2],
                  declared=[{"file": "include/buffer.h", "span": {"__span__": [18, 8, 18, 21]}}])]
    fn = {"id": 0, "file": "include/buffer.h", "name": "buffer_length", "line": 18}
    assert runtime.map_function(fn, graph(nodes, []), "c") == ("f", None)


def test_ambiguity_is_broken_by_line_then_by_qualifier():
    nodes = [
        node("one", "run", "a.go", [1, 1, 5, 2], kind="method", qn="p.A.run"),
        node("two", "run", "a.go", [7, 1, 9, 2], kind="method", qn="p.B.run"),
        node("three", "get", "b.go", None, kind="method", qn="p.A.get"),
        node("four", "get", "b.go", None, kind="method", qn="p.B.get"),
    ]
    g = graph(nodes, [])
    by_line = {"id": 0, "file": "a.go", "name": "p.run", "line": 8}
    assert runtime.map_function(by_line, g, "go") == ("two", None)
    by_qualifier = {"id": 0, "file": "b.go", "name": "p.(*B).get", "line": 0}
    assert runtime.map_function(by_qualifier, g, "go") == ("four", None)
    unknown = {"id": 0, "file": "b.go", "name": "get", "line": 0}
    assert runtime.map_function(unknown, g, "go") == (None, "ambiguous")


@pytest.mark.parametrize(("language", "raw", "expected"), [
    ("cpp", "Foo<int>::bar(int, char const*) const", (["Foo", "bar"], False)),
    ("cpp", "std::vector<int>::operator[](unsigned long)", (["std", "vector", "operator[]"], False)),
    ("cpp", "Foo::operator<<(int)", (["Foo", "operator<<"], False)),
    ("rust", "<proj::Foo as core::ops::drop::Drop>::drop", (["proj", "Foo", "drop"], False)),
    ("rust", "proj::run::{{closure}}", (["proj", "run"], True)),
    ("go", "github.com/x/gin.(*Engine).Run.func1", (["github", "com/x/gin", "Engine", "Run"], True)),
    ("go", "pkg.Map[...]", (["pkg", "Map"], False)),
    ("python", "a.f.<locals>.<lambda>", (["a", "f"], True)),
    ("php", "App\\Foo->bar", (["App", "Foo", "bar"], False)),
    ("c", "main", (["main"], False)),
])
def test_split_name(language, raw, expected):
    assert runtime.split_name(raw, language) == expected


def test_raise_floors_never_lowers():
    before = {"o/r": {"recall": 0.6}}
    assert runtime.raise_floors(before, [{"project": "o/r", "recall": 0.5}]) == before
    assert runtime.raise_floors(before, [{"project": "o/r", "recall": 0.7}]) == {
        "o/r": {"recall": 0.7}
    }
    assert runtime.raise_floors({}, [{"project": "n/p", "recall": 0.25}]) == {
        "n/p": {"recall": 0.25}
    }
    assert runtime.raise_floors(before, [{"project": "o/r", "recall": None}]) == before


def test_check_and_update_commands(tmp_path, monkeypatch, capsys):
    report = tmp_path / "report.json"
    thresholds = tmp_path / "thresholds.json"
    monkeypatch.setattr(runtime, "REPORT", report)
    monkeypatch.setattr(runtime, "THRESHOLDS", thresholds)
    monkeypatch.setattr(runtime, "_artifacts", lambda only: [(None, {"project": "o/r"})])
    args = argparse.Namespace(project=None)

    report.write_text(json.dumps({"projects": [{"project": "o/r", "recall": 0.5}]}))
    thresholds.write_text("{}\n")
    assert runtime.cmd_check(args) == 1  # no floor yet
    assert runtime.cmd_update(args) == 0
    assert json.loads(thresholds.read_text()) == {"o/r": {"recall": 0.5}}
    assert runtime.cmd_check(args) == 0

    report.write_text(json.dumps({"projects": [{"project": "o/r", "recall": 0.4}]}))
    assert runtime.cmd_check(args) == 1  # below the floor
    assert runtime.cmd_update(args) == 0
    assert json.loads(thresholds.read_text()) == {"o/r": {"recall": 0.5}}  # not lowered
    assert "below the floor" in capsys.readouterr().out


def test_artifacts_round_trip_through_the_scorer(tmp_path):
    art = common.build_artifact(
        project="o/r", ref="1", sha="s", language="c", tool="t", workload="w", command="c",
        calls=[(("x.c", "a", 1), ("x.c", "b", 5), 2)],
    )
    path = tmp_path / "o__r.json"
    common.write_artifact(path, art)
    g = graph(simple(), [("a", "b", "definition")])
    assert runtime.score(common.load_artifact(path), g)["recall"] == 1.0
