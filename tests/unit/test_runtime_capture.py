"""The runtime-artifact capture helpers: callgrind parsing and the artifact writer.

Pure Python on synthetic text: no clone, no toolchain, no valgrind.
"""

from __future__ import annotations

import json
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

CAPTURE = Path(__file__).resolve().parents[1] / "runtime" / "capture"
sys.path.insert(0, str(CAPTURE))

import callgrind  # noqa: E402
import common  # noqa: E402
import go  # noqa: E402

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


def test_callgrind_call_target_does_not_move_the_call_site(tmp_path):
    # `calls=` names the callee's position relative to the call site, and the
    # cost lines after it stay relative to the call site: f must keep line 50.
    text = (
        "positions: line\nevents: Ir\n"
        "fl=(1) /proj/a.c\nfn=(1) f\n50 1\n+1 1\n"
        "cfn=(2) g\ncalls=1 10\n* 1\n+1 1\n"
        "cfn=(2)\ncalls=1 -42\n* 1\n"
        "fn=(2)\n10 1\n"
    )
    calls = callgrind.parse([_write(tmp_path, text)], "/proj")
    assert calls == [(("a.c", "f", 50), ("a.c", "g", 10), 2)]


def test_callgrind_drops_relative_paths_outside_the_root(tmp_path):
    # glibc's debug info names `./csu/./csu/init-first.c`, relative to no
    # directory callgrind knows; a relative path counts only when it is a file
    # under the root.
    root = tmp_path / "proj"
    (root / "src").mkdir(parents=True)
    (root / "src" / "main.c").write_text("int main(void) { return 0; }\n")
    text = (
        "positions: line\nevents: Ir\n"
        "fl=(1) ./csu/./csu/init-first.c\nfn=(1) _init_first\n58 1\n"
        "cfl=(2) src/main.c\ncfn=(2) main\ncalls=1 1\n* 1\n"
        "fl=(2)\nfn=(2)\n1 1\ncfl=(1)\ncfn=(3) exit\ncalls=1 1\n* 1\n"
        "cfn=(2)\ncalls=1 1\n* 1\n"
    )
    calls = callgrind.parse([_write(tmp_path, text)], str(root))
    assert calls == [(("src/main.c", "main", 1), ("src/main.c", "main", 1), 1)]


def test_rust_capture_keeps_only_functions_written_in_the_project():
    import rust

    crates = {"grep_cli", "grep_matcher", "globset"}
    keep = rust.keep_function
    assert keep("a.rs", "grep_cli::wtr::stdout", 3, crates, "pub fn stdout() {")
    assert keep("a.rs", "<&mut M as grep_matcher::Matcher>::find_at", 9, crates, "fn find_at(")
    assert keep("a.rs", "<grep_cli::X as termcolor::WriteColor>::reset", 9, crates, "fn reset(")
    assert not keep("a.rs", "alloc::boxed::box_new_uninit", 0, crates, "")
    assert not keep(
        "a.rs", "<core::ptr::non_null::NonNull<_>>::new_unchecked::precondition_check",
        0, crates, "")
    assert not keep(
        "a.rs", "<core::ops::range::RangeFrom<usize> as core::slice::index::SliceIndex<[u8]>>::index",
        0, crates, "")
    # #[derive(Clone, Default)] impls sit on the attribute or the item, not on a `fn`.
    assert not keep("a.rs", "<globset::Tokens as core::default::Default>::default", 222,
                    crates, "#[derive(Clone, Debug, Default, Eq, PartialEq)]")
    assert not keep("a.rs", "<grep_cli::B as core::default::Default>::default", 197,
                    crates, "pub struct B {")
    assert keep("a.rs", "<grep_cli::C as core::default::Default>::default", 157,
                crates, "    fn default() -> C {")

GO_TRACE = (
    "github.com/o/r.(*Context).Next\t/w/src/context.go\t188\t"
    "github.com/o/r.TestX.func1\t/w/src/context_test.go\t212\t3\n"
    "testing.tRunner\t/go/src/testing/testing.go\t1876\t"
    "github.com/o/r.TestX\t/w/src/context_test.go\t200\t1\n"
    "github.com/o/r/binding.validate\t/w/src/binding/binding.go\t40\t"
    "github.com/o/r/binding.(*defaultValidator).ValidateStruct\t/w/src/binding/default_validator.go\t52\t9\n"
    "github.com/o/r.TestMain\t/w/src/zz_rttrace_main_test.go\t9\t"
    "github.com/o/r/internal/rttrace.Dump\t/w/src/internal/rttrace/rttrace.go\t60\t1\n"
    "github.com/o/r.helper\t/w/src/a.go\t5\t"
    "github.com/dep/x.Do\t/root/go/pkg/mod/github.com/dep/x@v1/x.go\t3\t2\n"
)


@pytest.mark.parametrize(("raw", "short"), [
    ("github.com/gin-gonic/gin.(*Context).Next", "gin.(*Context).Next"),
    ("github.com/gin-gonic/gin/binding.mapForm", "binding.mapForm"),
    ("github.com/gin-gonic/gin_test.TestX.func1.2", "gin_test.TestX.func1.2"),
    ("github.com/o/r/internal/x.F[...]", "x.F[...]"),
    ("main.main", "main.main"),
])
def test_go_short_name_keeps_the_package_element(raw, short):
    assert go.short_name(raw) == short


def test_go_trace_keeps_project_calls_and_drops_the_injected_hook():
    calls = list(go.parse_trace(GO_TRACE.splitlines(keepends=True), "/w/src"))
    assert calls == [
        (("context.go", "r.(*Context).Next", 188), ("context_test.go", "r.TestX.func1", 212), 3),
        (("binding/binding.go", "binding.validate", 40),
         ("binding/default_validator.go", "binding.(*defaultValidator).ValidateStruct", 52), 9),
    ]


def test_go_trace_rejects_a_malformed_row():
    with pytest.raises(ValueError):
        list(go.parse_trace(["a\tb\t1\n"], "/w/src"))


def test_go_trace_builds_a_valid_artifact(tmp_path):
    calls = list(go.parse_trace(GO_TRACE.splitlines(keepends=True), "/w/src"))
    art = common.build_artifact(
        project="o/r", ref="v1", sha="abc", language="go", tool="t",
        workload="w", command="c", calls=calls,
    )
    path = tmp_path / "o__r.json"
    common.write_artifact(path, art)
    assert [f["name"] for f in common.load_artifact(path)["functions"]] == [
        "binding.validate", "binding.(*defaultValidator).ValidateStruct",
        "r.(*Context).Next", "r.TestX.func1",
    ]

PY_SAMPLE = '''
    import contextlib
    import functools
    import json


    def deco(func):
        @functools.wraps(func)
        def wrapper(*args):
            return func(*args)
        return wrapper


    class Base:
        def __init__(self):
            self.x = 1


    def helper():
        return json.dumps([1])


    @deco
    def decorated(n):
        return sorted([n, 2], key=lambda v: -v)


    @contextlib.contextmanager
    def cm():
        helper()
        yield


    def run():
        Base()
        decorated(3)
        with cm():
            pass
        return list(helper() for _ in range(2))


    VALUE = helper()
'''


@pytest.mark.skipif(sys.version_info < (3, 12), reason="sys.monitoring needs Python 3.12")
def test_python_trace_records_project_calls_only(tmp_path):
    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "__init__.py").write_text("")
    (tmp_path / "pkg" / "a.py").write_text(textwrap.dedent(PY_SAMPLE))
    (tmp_path / "main.py").write_text("from pkg.a import run\nrun()\n")
    raw = tmp_path / "raw.json"
    subprocess.run(
        [sys.executable, str(CAPTURE / "python.py"), "trace", "--root", str(tmp_path),
         "--raw", str(raw), "--", "main.py"],
        cwd=tmp_path, check=True,
    )
    rows = json.loads(raw.read_text())["edges"]
    edges = {(r[1], r[4]) for r in rows}
    lines = {r[4]: r[5] for r in rows}
    assert ("run", "Base.__init__") in edges
    assert ("run", "deco.<locals>.wrapper") in edges
    assert ("deco.<locals>.wrapper", "decorated") in edges
    assert ("decorated", "decorated.<locals>.<lambda>") in edges
    assert ("run", "cm") in edges
    assert ("run", "run.<locals>.<genexpr>") in edges
    assert ("run.<locals>.<genexpr>", "helper") in edges
    assert not any("<module>" in a or "<module>" in b for a, b in edges)
    assert lines["decorated"] == 24
    assert lines["deco.<locals>.wrapper"] == 9


def test_python_convert_round_trip(tmp_path):
    raw = tmp_path / "raw.json"
    raw.write_text(json.dumps({
        "python": "3.12.3",
        "edges": [["a.py", "f", 1, "b.py", "g", 5, 2], ["a.py", "f", 1, "b.py", "g", 5, 3]],
    }))
    out = tmp_path / "o__r.json"
    subprocess.run(
        [sys.executable, str(CAPTURE / "python.py"), "convert", str(raw),
         "--root", str(tmp_path), "--project", "o/r", "--ref", "1", "--sha", "abc",
         "--workload", "w", "--command", "c", "-o", str(out)],
        check=True,
    )
    art = common.load_artifact(out)
    assert art["language"] == "python" and "sys.monitoring" in art["tool"]
    assert len(art["functions"]) == 2
    assert art["edges"] == [[0, 1, 5]]

# ---------------------------------------------------------------------------
# Xdebug (PHP) and composer_git
# ---------------------------------------------------------------------------

import composer_git  # noqa: E402
import xdebug  # noqa: E402

XDEBUG_HEADER = (
    "version: 1\ncreator: xdebug 3.2.0 (PHP 8.3.6)\npositions: line\n\n"
    "events: Time_(10ns) Memory_(bytes)\n\n"
)


def _block(fl, fn, line, calls=()):
    out = [f"fl={fl}", f"fn={fn}", f"{line} 1 0"]
    for cfl, cfn in calls:
        out += [f"cfl={cfl}", f"cfn={cfn}", "calls=1 0 0", f"{line} 1 0"]
    return "\n".join(out) + "\n\n"


def _profile(tmp_path, *blocks, name="cachegrind.out.t"):
    path = tmp_path / name
    path.write_text(XDEBUG_HEADER + "".join(blocks) + "summary: 1 2\n", encoding="utf-8")
    return path


def _php(tmp_path):
    root = tmp_path / "proj"
    (root / "src").mkdir(parents=True)
    return root, str(root / "src" / "A.php"), str(root / "src" / "B.php")


def test_xdebug_blocks_resolve_id_compression(tmp_path):
    text = (
        XDEBUG_HEADER
        + "fl=(1) /p/a.php\nfn=(1) A->x\n3 1 0\n\n"
        + "fl=(1)\nfn=(2) A->y\n9 1 0\ncfl=(1)\ncfn=(1)\ncalls=1 0 0\n9 1 0\n\n"
    )
    path = tmp_path / "c.out"
    path.write_text(text, encoding="utf-8")
    found = list(xdebug.blocks(path))
    assert [(b["fl"], b["fn"], b["line"]) for b in found] == [
        ("/p/a.php", "A->x", 3), ("/p/a.php", "A->y", 9),
    ]
    assert found[1]["calls"] == [("/p/a.php", "A->x")]


def test_xdebug_rebuilds_the_call_tree_in_post_order(tmp_path):
    _root, a, b = _php(tmp_path)
    path = _profile(
        tmp_path,
        _block(a, "A->y", 20),
        _block(b, "B->z", 30),
        _block(a, "A->x", 10, [(a, "A->y"), (b, "B->z")]),
        _block(a, "{main}", 1, [(a, "A->x")]),
    )
    counts, lines = xdebug.edges(path)
    assert counts[((a, "A->x"), (a, "A->y"))] == 1
    assert counts[((a, "A->x"), (b, "B->z"))] == 1
    assert counts[((a, "{main}"), (a, "A->x"))] == 1
    assert lines[(a, "A->x")] == 10


def test_xdebug_name_mismatch_raises(tmp_path):
    _root, a, b = _php(tmp_path)
    path = _profile(
        tmp_path,
        _block(b, "B->z", 30),
        _block(a, "A->x", 10, [(a, "A->y")]),
    )
    with pytest.raises(ValueError, match="mismatch"):
        xdebug.edges(path)


def test_xdebug_sums_call_instances(tmp_path):
    root, a, _b = _php(tmp_path)
    path = _profile(
        tmp_path,
        _block(a, "A->y", 20),
        _block(a, "A->y", 20),
        _block(a, "A->x", 10, [(a, "A->y"), (a, "A->y")]),
    )
    triples = xdebug.reduce([path], root)
    assert triples == [[["src/A.php", "A->x", 10], ["src/A.php", "A->y", 20], 2]]


def test_xdebug_bridges_only_through_callable_builtins(tmp_path):
    root, a, b = _php(tmp_path)
    internal = "php:internal"
    cuf = "php::call_user_func:{/x.php:3}"
    path = _profile(
        tmp_path,
        _block(b, "B->cb", 5),
        _block(internal, "php::array_map", 0, [(b, "B->cb")]),
        _block(b, "B->viaCuf", 6),
        _block(internal, cuf, 0, [(b, "B->viaCuf")]),
        _block(b, "B->viaCount", 7),
        _block(internal, "php::count", 0, [(b, "B->viaCount")]),
        _block(a, "A->go", 1, [
            (internal, "php::array_map"), (internal, cuf), (internal, "php::count"),
        ]),
        _block(b, "B->deep", 8),
        _block(internal, "php::count", 0, [(b, "B->deep")]),
        _block(internal, "php::array_map", 0, [(internal, "php::count")]),
        _block(a, "A->go2", 2, [(internal, "php::array_map")]),
    )
    triples = xdebug.reduce([path], root)
    pairs = sorted((t[0][1], t[1][1]) for t in triples)
    assert pairs == [("A->go", "B->cb"), ("A->go", "B->viaCuf")]


def test_xdebug_drops_pseudo_functions_eval_and_vendor(tmp_path):
    root, a, b = _php(tmp_path)
    vendor = str(root / "vendor" / "x" / "V.php")
    evald = str(root / "src" / "A.php(9) : eval()'d code")
    path = _profile(
        tmp_path,
        _block(a, "A->kept", 20),
        _block(vendor, "V->lib", 3),
        _block(evald, "evalled", 1),
        _block(a, "A->caller", 10, [(a, "A->kept"), (vendor, "V->lib"), (evald, "evalled")]),
        _block(b, "require_once::/p/x.php", 1, [(a, "A->caller")]),
        _block(a, "{main}", 1, [(b, "require_once::/p/x.php")]),
    )
    triples = xdebug.reduce([path], root)
    assert [(t[0][1], t[1][1]) for t in triples] == [("A->caller", "A->kept")]


def test_xdebug_normalizes_closures_and_anonymous_classes(tmp_path):
    root, a, _b = _php(tmp_path)
    closure = f"App\\A->App\\{{closure:{a}:12-14}}"
    anonymous = f"{{anonymous-class:{a}:30-40}}->run"
    php84 = f"App\\{{closure:{a}:7}}"
    assert xdebug.normalize(closure) == "App\\A->App\\{closure:12-14}"
    assert xdebug.normalize(anonymous) == "{anonymous-class:30-40}->run"
    assert xdebug.normalize(php84) == "App\\{closure:7}"
    path = _profile(
        tmp_path,
        _block(a, closure, 12),
        _block(a, anonymous, 31),
        _block(a, "A->go", 1, [(a, closure), (a, anonymous)]),
    )
    text = json.dumps(xdebug.reduce([path], root))
    assert str(tmp_path) not in text
    assert "{closure:12-14}" in text


def test_xdebug_trampoline_is_recorded_as_the_magic_method(tmp_path):
    root, a, b = _php(tmp_path)
    Path(a).write_text(
        "<?php\nclass X {\n    public function __call($m, $a)\n    {\n    }\n"
        "    public static function __callStatic($m, $a) {}\n}\n",
        encoding="utf-8",
    )
    assert xdebug.trampoline(a, "X->foo", 3) == "X->__call"
    assert xdebug.trampoline(a, "X->bar", 6) == "X::__callStatic"
    assert xdebug.trampoline(a, "X->plain", 2) == "X->plain"
    assert xdebug.trampoline(a, "X->none", 0) == "X->none"
    path = _profile(
        tmp_path,
        _block(b, "B->real", 5),
        _block(a, "X->foo", 3, [(b, "B->real")]),
        _block(b, "B->go", 9, [(a, "X->foo")]),
    )
    triples = xdebug.reduce([path], root)
    assert {(t[0][1], t[1][1]) for t in triples} == {
        ("B->go", "X->__call"), ("X->__call", "B->real"),
    }


def test_xdebug_reduce_and_build_make_a_valid_deterministic_artifact(tmp_path):
    root, a, b = _php(tmp_path)
    one = _profile(
        tmp_path, _block(b, "B->z", 30), _block(a, "A->x", 10, [(b, "B->z")]), name="one.out"
    )
    two = _profile(
        tmp_path, _block(b, "B->z", 30), _block(a, "A->x", 10, [(b, "B->z")]), name="two.out"
    )
    parts = []
    for index, profile in enumerate((one, two)):
        part = tmp_path / f"part{index}.json"
        assert xdebug.main(["reduce", str(profile), "--root", str(root),
                                     "-o", str(part)]) == 0
        parts.append(str(part))
    outputs = []
    for name in ("art1.json", "art2.json"):
        out = tmp_path / name
        assert xdebug.main([
            "build", *parts, "--root", str(root), "--project", "o/r", "--ref", "v1",
            "--tool", "xdebug", "--workload", "w", "--command", "c", "-o", str(out),
        ]) == 0
        outputs.append(out.read_text(encoding="utf-8"))
    assert outputs[0] == outputs[1]
    artifact = json.loads(outputs[0])
    assert common.validate(artifact) == []
    assert artifact["language"] == "php"
    assert artifact["edges"] == [[0, 1, 2]]


def test_composer_git_writes_installed_json(tmp_path, monkeypatch):
    lock = {
        "packages": [{"name": "a/b", "version": "1.0.0", "source": {"url": "u", "reference": "r"}}],
        "packages-dev": [{"name": "c/d", "version": "2.0.0", "source": {"url": "u", "reference": "r"}}],
    }
    (tmp_path / "composer.lock").write_text(json.dumps(lock), encoding="utf-8")
    fetched = []

    def fake_fetch(package, vendor):
        fetched.append(package["name"])
        (vendor / package["name"]).mkdir(parents=True)
        return None

    monkeypatch.setattr(composer_git, "fetch", fake_fetch)
    assert composer_git.install(tmp_path, jobs=1) == []
    assert sorted(fetched) == ["a/b", "c/d"]
    installed = json.loads((tmp_path / "vendor" / "composer" / "installed.json").read_text())
    assert [p["install-path"] for p in installed["packages"]] == ["../a/b", "../c/d"]
    assert installed["dev"] is True
    assert installed["dev-package-names"] == ["c/d"]


def test_composer_git_reports_a_package_without_source(tmp_path):
    lock = {"packages": [{"name": "a/b", "version": "1"}], "packages-dev": []}
    (tmp_path / "composer.lock").write_text(json.dumps(lock), encoding="utf-8")
    assert composer_git.install(tmp_path) == ["a/b: no git source"]


def test_jsprobe_traces_become_an_artifact(tmp_path):
    import jsprobe  # noqa: PLC0415

    traces = tmp_path / "trace"
    traces.mkdir()
    a = ["src/a.ts", "<anonymous>", 3]
    f = ["src/b.ts", "f", 10]
    dep = ["node_modules/x/index.ts", "g", 1]
    (traces / "1-0.jsonl").write_text(
        json.dumps([a, f, 2]) + "\n" + json.dumps([f, dep, 5]) + "\n", encoding="utf-8")
    (traces / "2-0.jsonl").write_text(json.dumps([a, f, 3]) + "\n\n", encoding="utf-8")
    calls = list(jsprobe.read_traces([traces], tmp_path))
    art = common.build_artifact(
        project="o/r", ref="v1", sha="abc", language="typescript", tool="t",
        workload="w", command="c", calls=calls,
    )
    assert common.validate(art) == []
    assert [(x["file"], x["name"], x["line"]) for x in art["functions"]] == [
        ("src/a.ts", "<anonymous>", 3), ("src/b.ts", "f", 10),
    ]
    assert art["edges"] == [[0, 1, 5]]
