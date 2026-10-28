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
    ("python", "pkg.Foo.__init__", False),
    ("python", "pkg.Foo.__eq__", True),
    ("python", "pkg.Foo.run", False),
    ("rust", "<proj::Foo as core::ops::drop::Drop>::drop", True),
    ("rust", "<proj::Foo as proj::Shape>::area", False),
    ("rust", "<alloc::vec::Vec<T> as core::iter::traits::iterator::Iterator>::next", True),
    ("php", "App\\Foo->__construct", True),
    ("php", "__", False),
    ("php", "App\\C->offsetGet", True),
    ("php", "App\\C->getIterator", True),
    ("php", "App\\C->__callStatic", True),
    ("php", "App\\C->count", False),
    ("typescript", "Foo.constructor", True),
    ("go", "pkg.init", True),
    ("c", "main", False),
    ("cpp", "ns::Foo::run(int)", False),
    ("cpp", "_GLOBAL__sub_I__ZN7leveldb4DBTestE", True),
    ("cpp", "__static_initialization_and_destruction_0()", True),
    ("cpp", "leveldb::DBTest_Empty_Test::TestBody()", False),
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


def test_php_closure_maps_to_its_enclosing_method_and_folds_into_it():
    nodes = [
        node("m", "m", "a.php", [10, 1, 20, 2], kind="method", qn="App\\C.m"),
        node("h", "h", "a.php", [30, 1, 32, 2], kind="method", qn="App\\C.h"),
    ]
    g = graph(nodes, [("m", "h", "definition")])
    art = artifact("php", [
        ("a.php", "App\\C->m", 10), ("a.php", "App\\C->App\\{closure:12-14}", 12),
        ("a.php", "App\\C->h", 30),
    ], [(0, 1, 1), (1, 2, 4)])
    result = runtime.score(art, g)
    assert result["folded"] == 1
    assert result["found"] == 1
    assert result["unmapped"] == 0
    top = {"id": 0, "file": "a.php", "name": "{closure:50-52}", "line": 50}
    assert runtime.map_function(top, g, "php") == (None, "no_name")


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
    ("cpp", "leveldb::Slice::ToString[abi:cxx11]() const", (["leveldb", "Slice", "ToString"], False)),
    ("rust", "<proj::Foo as core::ops::drop::Drop>::drop", (["proj", "Foo", "drop"], False)),
    ("rust", "proj::run::{{closure}}", (["proj", "run"], True)),
    ("go", "github.com/x/gin.(*Engine).Run.func1", (["github", "com/x/gin", "Engine", "Run"], True)),
    ("go", "pkg.Map[...]", (["pkg", "Map"], False)),
    ("python", "a.f.<locals>.<lambda>", (["a", "f"], True)),
    ("python", "Cls.run.<locals>.helper", (["Cls", "run", "helper"], False)),
    ("python", "f.<locals>.<genexpr>", (["f"], True)),
    ("python", "t.<locals>.<lambda>.<locals>.<lambda>", (["t"], True)),
    ("php", "App\\Foo->bar", (["App", "Foo", "bar"], False)),
    ("php", "App\\Foo->App\\{closure:12-14}", (["App", "Foo"], True)),
    ("php", "App\\{closure:3-5}", (["App"], True)),
    ("php", "{closure:3-5}", ([], True)),
    ("php", "App\\Foo::App\\{closure:7}", (["App", "Foo"], True)),
    ("php", "{anonymous-class:8-20}->passes", (["{anonymous-class:8-20}", "passes"], False)),
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


def test_gtest_body_maps_to_its_test_macro():
    nodes = [
        node("e", "TEST_F", "t.cc", [1, 1, 3, 2], qn="leveldb::TEST_F(DBTest,Empty)"),
        node("o", "TEST_F", "t.cc", [5, 1, 7, 2], qn="leveldb::TEST_F(DBTest,Other)"),
        node("b", "TEST", "t.cc", [9, 1, 11, 2], qn="leveldb::TEST(Foo,Bar_Baz)"),
    ]
    g = graph(nodes, [])

    def fn(name):
        return {"id": 0, "file": "t.cc", "name": name, "line": 0}

    assert runtime.map_function(fn("leveldb::DBTest_Empty_Test::TestBody()"), g, "cpp") == ("e", None)
    assert runtime.map_function(fn("leveldb::Foo_Bar_Baz_Test::TestBody()"), g, "cpp") == ("b", None)
    assert runtime.map_function(fn("leveldb::DBTest_Missing_Test::TestBody()"), g, "cpp") == (None, "no_name")


def test_analyze_langs():
    assert runtime.analyze_langs("cpp") == "cpp,c"
    assert runtime.analyze_langs("go") == "go"

def test_rust_raw_identifier_maps_to_its_r_hash_node():
    nodes = [node("m", "r#async", "src/p.rs", [10, 5, 12, 6], kind="method")]
    fn = {"id": 0, "file": "src/p.rs", "name": "<p::StderrReader>::async", "line": 10}
    assert runtime.map_function(fn, graph(nodes, []), "rust") == ("m", None)


def test_rust_function_generated_by_a_project_macro_maps_to_the_macro():
    macro = node("mac", "syntax", "src/g.rs", [29, 5, 37, 6])
    macro["metadata"]["macro"] = "macro_rules"
    plain = node("f", "helper", "src/g.rs", [1, 1, 100, 2])
    nodes = [macro, plain]
    fn = {"id": 0, "file": "src/g.rs", "name": "g::tests::any1", "line": 32}
    assert runtime.map_function(fn, graph(nodes, []), "rust") == ("mac", None)
    # Only a macro catches a named function: no line-only fallback otherwise.
    fn = {"id": 0, "file": "src/g.rs", "name": "g::tests::other", "line": 60}
    assert runtime.map_function(fn, graph(nodes, []), "rust") == (None, "no_name")

@pytest.mark.parametrize(("raw", "expected"), [
    ("gin.init.0", (["gin", "init"], False)),
    ("gin.init.0.func1", (["gin", "init"], True)),
    ("gin.init.func1", (["gin", "init"], True)),
    ("gin.(*Context).initFormCache", (["gin", "Context", "initFormCache"], False)),
])
def test_go_init_names(raw, expected):
    assert runtime.split_name(raw, "go") == expected


def test_go_init_rules():
    assert runtime.is_implicit("gin.init.3", "go")
    assert not runtime.is_implicit("gin.init.func1", "go")
    nodes = [
        node("init", "init", "a.go", [1, 1, 3, 2], qn="p.init"),
        node("fmt", "defaultFormatter", "b.go", [5, 1, 9, 2], kind="variable", qn="p.defaultFormatter"),
        node("set", "SetMode", "b.go", [11, 1, 12, 2], qn="p.SetMode"),
        node("log", "Logger", "b.go", [14, 1, 20, 2], qn="p.Logger"),
    ]
    g = graph(nodes, [("init", "set", "definition"), ("log", "fmt", "value")])
    art = artifact("go", [
        ("b_test.go", "p.init.1", 3), ("b.go", "p.SetMode", 11),
        ("b.go", "p.init.func1", 5), ("b.go", "p.Logger", 14),
    ], [(0, 1, 1), (3, 2, 4)])
    result = runtime.score(art, g)
    assert (result["found"], result["unmapped"], result["implicit"]) == (2, 0, 0)

def _pnode(ident, name, file, span, kind="function", qn=None, meta=None):
    return {
        "id": ident, "kind": kind, "qualified_name": qn or name, "name": name,
        "file_path": file, "span": span, "metadata": meta or {},
    }


def _part(functions, edges):
    return {
        "schema": 1, "project": "o/r", "ref": "1", "sha": "abc", "language": "python",
        "tool": "t", "workload": "w", "command": "c",
        "functions": [
            {"id": i, "file": f, "name": n, "line": ln}
            for i, (f, n, ln) in enumerate(functions)
        ],
        "edges": [list(e) for e in edges],
    }


def test_python_lambda_maps_to_its_node_and_genexpr_to_the_function_not_a_variable():
    nodes = [
        _pnode("f", "f", "a.py", [1, 1, 10, 2]),
        _pnode("lam", "<lambda1>", "a.py", [3, 20, 3, 40], qn="a.f.<lambda1>"),
        _pnode("v", "names", "a.py", [5, 5, 5, 60], kind="variable", qn="a.f.names"),
    ]
    g = runtime.Graph({"nodes": nodes, "relations": []})
    lam = {"id": 0, "file": "a.py", "name": "f.<locals>.<lambda>", "line": 3}
    gen = {"id": 1, "file": "a.py", "name": "f.<locals>.<genexpr>", "line": 5}
    assert runtime.map_function(lam, g, "python") == ("lam", None)
    assert runtime.map_function(gen, g, "python") == ("f", None)


def test_python_property_callee_is_implicit():
    nodes = [
        _pnode("m", "m", "a.py", [1, 1, 5, 2], kind="method", qn="a.C.m"),
        _pnode("p", "size", "a.py", [7, 1, 9, 2], kind="method", qn="a.C.size",
               meta={"is_property": True, "decorators": ["property"]}),
        _pnode("s", "size2", "a.py", [11, 1, 13, 2], kind="method", qn="a.C.size2",
               meta={"decorators": ["size2.setter"]}),
    ]
    g = runtime.Graph({"nodes": nodes, "relations": []})
    art = _part([("a.py", "C.m", 1), ("a.py", "C.size", 7), ("a.py", "C.size2", 11)],
                [(0, 1, 1), (0, 2, 1)])
    result = runtime.score(art, g)
    assert result["implicit"] == 2 and result["missing"] == 0


def test_python_call_to_a_class_is_a_call_to_its_own_or_inherited_init():
    nodes = [
        _pnode("main", "main", "a.py", [1, 1, 5, 2]),
        _pnode("A", "A", "a.py", [7, 1, 9, 2], kind="class", qn="a.A"),
        _pnode("B", "B", "a.py", [11, 1, 15, 2], kind="class", qn="a.B"),
        _pnode("binit", "__init__", "a.py", [12, 5, 13, 2], kind="method", qn="a.B.__init__"),
    ]
    rel = [
        {"source_id": "A", "target_id": "B", "kind": "inherits_from", "metadata": {}},
        {"source_id": "main", "target_id": "A", "kind": "calls",
         "metadata": {"resolution": "definition"}},
    ]
    g = runtime.Graph({"nodes": nodes, "relations": rel})
    art = _part([("a.py", "main", 1), ("a.py", "B.__init__", 12)], [(0, 1, 1)])
    result = runtime.score(art, g)
    assert result["found"] == 1 and result["implicit"] == 0

# --- TypeScript (colinhacks/zod) -------------------------------------------


@pytest.mark.parametrize(("raw", "expected"), [
    ("ZodType.parse", (["ZodType", "parse"], False)),
    ("ZodType.get description", (["ZodType", "description"], False)),
    ("$ZodType.<anonymous#3>", (["$ZodType"], True)),
    ("<anonymous>", ([], True)),
    ("defineLazy.<object>.get [computed]", (["defineLazy"], True)),
    ("_ZodString.<object#2>.regex.<anonymous>", (["_ZodString"], True)),
])
def test_split_name_typescript(raw, expected):
    assert runtime.split_name(raw, "typescript") == expected


@pytest.mark.parametrize(("name", "implicit"), [
    ("ZodType.get description", True),
    ("defineLazy.<object>.get [computed]", True),
    ("merge.def.<object>.set shape", True),
    ("Doc.constructor", True),
    ("ZodType.<object>.refine", False),
    ("getter", False),
])
def test_is_implicit_typescript(name, implicit):
    assert runtime.is_implicit(name, "typescript") is implicit


def test_typescript_closures_map_to_the_callable_or_the_file():
    nodes = [
        node("file", "a.ts", "a.ts", kind="file"),
        node("var", "$ZodType", "a.ts", [1, 14, 30, 3], kind="variable"),
        node("run", "runChecks", "a.ts", [5, 3, 9, 4]),
        node("m", "description", "a.ts", [40, 3, 42, 4], kind="method", qn="ZodType.description"),
    ]
    g = graph(nodes, [])
    fn = lambda name, line: {"file": "a.ts", "name": name, "line": line}  # noqa: E731
    # inside a FUNCTION: that function; in a top-level initializer: the FILE, never the VARIABLE
    assert runtime.map_function(fn("$ZodType.runChecks.<anonymous>", 6), g, "typescript") == ("run", None)
    assert runtime.map_function(fn("$ZodType.<anonymous#2>", 12), g, "typescript") == ("file", None)
    assert runtime.map_function(fn("$ZodType.<object>.check", 20), g, "typescript") == ("file", None)
    assert runtime.map_function(fn("ZodType.get description", 40), g, "typescript") == ("m", None)
    # a named function without a node stays unmapped: no line fallback
    assert runtime.map_function(fn("default", 50), g, "typescript") == (None, "no_name")


def test_an_accessor_folded_into_a_function_does_not_hide_a_written_call():
    nodes = [
        node("file", "a.ts", "a.ts", kind="file"),
        node("lazy", "defineLazy", "a.ts", [10, 1, 20, 2]),
    ]
    g = graph(nodes, [])
    art = artifact("typescript", [
        ("a.ts", "<anonymous>", 2), ("a.ts", "defineLazy", 10),
        ("a.ts", "defineLazy.<object>.get [computed]", 12),
    ], [(0, 1, 1), (0, 2, 5)])
    result = runtime.score(art, g)
    # both raw edges map to (file, defineLazy): the written call is scored, not implicit
    assert (result["implicit"], result["missing"]) == (0, 1)
