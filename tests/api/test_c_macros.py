"""What a macro of the project expands to, and what that makes called.

The grammar reads `CALL(x)` as a call of `CALL`; it is a call of whatever the
macro's body calls, placed at the invocation. A macro invoked where a declaration
goes declares what it expands to.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind


def analyse(tmp_path: Path, source: str, name: str = "main.c") -> callix.Graph:
    (tmp_path / name).write_text(source)
    adapter = callix.CppAdapter() if name.endswith(".cpp") else callix.CAdapter()
    return adapter.analyze(str(tmp_path))


def calls(graph: callix.Graph) -> set[tuple[str, str]]:
    return {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.CALLS
    }


def test_a_macro_that_expands_to_a_call(tmp_path: Path) -> None:
    graph = analyse(tmp_path, "int f(int x) { return x; }\n#define CALL(x) f(x)\nint main(void) { return CALL(1); }\n")
    found = calls(graph)
    assert ("main", "f") in found
    assert not any(target == "CALL" for _, target in found)


def test_a_parameter_called_as_a_function_is_the_argument(tmp_path: Path) -> None:
    graph = analyse(
        tmp_path,
        "int g(int x) { return x; }\n#define APPLY(fn, x) fn(x)\nint main(void) { return APPLY(g, 1); }\n",
    )
    assert ("main", "g") in calls(graph)


def test_macros_within_macros_and_object_like_macros(tmp_path: Path) -> None:
    graph = analyse(
        tmp_path,
        """int f(int x) { return x; }
int on_event(int x) { return x; }
#define INNER(x) f(x)
#define OUTER(x) INNER(x)
#define HANDLER on_event
int main(void) { OUTER(1); return HANDLER(2); }
""",
    )
    found = calls(graph)
    assert ("main", "f") in found
    assert ("main", "on_event") in found


def test_a_macro_named_like_the_function_it_wraps(tmp_path: Path) -> None:
    graph = analyse(
        tmp_path,
        "int f(int x) { return x; }\n#define f(x) f((x) + 1)\nint main(void) { return f(1); }\n",
    )
    assert ("main", "f") in calls(graph)


def test_token_pasting_names_the_function(tmp_path: Path) -> None:
    graph = analyse(
        tmp_path,
        "int f_one(void) { return 1; }\n#define CALL(n) f_##n()\nint main(void) { return CALL(one); }\n",
    )
    assert ("main", "f_one") in calls(graph)


def test_a_macro_that_defines_a_function(tmp_path: Path) -> None:
    for invocation in ("DEFINE_WRAPPER(wrapped);", "DEFINE_WRAPPER(wrapped)"):
        directory = tmp_path / invocation.rstrip(";").lower() / ("semi" if invocation.endswith(";") else "bare")
        directory.mkdir(parents=True)
        graph = analyse(
            directory,
            "int f(int x) { return x; }\n"
            "#define DEFINE_WRAPPER(name) int name(int x) { return f(x); }\n"
            f"{invocation}\n"
            "int main(void) { return wrapped(1); }\n",
        )
        functions = {n.qualified_name for n in graph.nodes.values() if n.kind is NodeKind.FUNCTION}
        assert "wrapped" in functions
        found = calls(graph)
        assert ("wrapped", "f") in found
        assert ("main", "wrapped") in found


def test_a_function_returned_by_a_function_is_called(tmp_path: Path) -> None:
    graph = analyse(
        tmp_path,
        "int one(int x) { return x; }\ntypedef int (*h)(int);\nh pick(void) { return one; }\nint main(void) { return pick()(1); }\n",
    )
    assert ("main", "one") in calls(graph)


def test_a_local_object_is_destroyed_when_its_scope_ends(tmp_path: Path) -> None:
    graph = analyse(
        tmp_path,
        "struct R { ~R() {} };\nstruct Plain {};\nvoid use() { R r; Plain p; }\nint main() { use(); return 0; }\n",
        "main.cpp",
    )
    found = calls(graph)
    assert ("use()", "R::~R()") in found
    assert not any("~Plain" in target for _, target in found)


def test_a_thread_runs_the_function_it_is_given(tmp_path: Path) -> None:
    graph = analyse(
        tmp_path,
        "#include <thread>\nvoid worker() {}\nint main() { std::thread t(worker); t.join(); return 0; }\n",
        "main.cpp",
    )
    assert ("main()", "worker()") in calls(graph)
