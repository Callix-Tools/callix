"""Which function a TypeScript call reaches when the callee is a value.

Destructuring, keys written as literals, `bind`, `call` and `apply`, a property
the constructor assigns, an object handed to a destructured parameter, and the
library functions that call what they are given.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import RelationKind


def calls(tmp_path: Path, source: str) -> set[tuple[str, str]]:
    (tmp_path / "main.ts").write_text(source)
    graph = callix.TypeScriptAdapter().analyze(str(tmp_path))
    return {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.CALLS
    }


def test_destructuring_follows_the_key_and_the_position(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """function a() {}
function b() {}
const o = { x: a, y: b };
const { y } = o;
y();
const [first, second] = [a, b];
second();
""",
    )
    assert ("main.ts", "main.b") in found
    assert ("main.ts", "main.a") not in found


def test_a_literal_key_or_index_picks_the_element(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """function one() {}
function two() {}
const fns = [one, two];
fns[1]();
""",
    )
    assert ("main.ts", "main.two") in found
    assert ("main.ts", "main.one") not in found


def test_bind_call_and_apply_reach_the_function(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """function f() {}
function g() {}
function h() {}
const bound = f.bind(null);
bound();
g.call(null);
h.apply(null, []);
""",
    )
    assert {("main.ts", "main.f"), ("main.ts", "main.g"), ("main.ts", "main.h")} <= found


def test_a_property_the_constructor_assigns_is_called_by_a_method(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """function cb() {}
class Job {
    private f: () => void;
    constructor(f: () => void) { this.f = f; }
    run() { this.f(); }
}
new Job(cb).run();
""",
    )
    assert ("main.Job.run", "main.cb") in found


def test_an_object_handed_to_a_destructured_parameter(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """function cb() {}
function other() {}
function run({ handler }: { handler: () => void }) { handler(); }
run({ handler: cb });
""",
    )
    assert ("main.run", "main.cb") in found
    assert ("main.run", "main.other") not in found


def test_a_default_parameter_is_what_the_parameter_holds(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """function fallback() {}
function run(cb = fallback) { cb(); }
run();
""",
    )
    assert ("main.run", "main.fallback") in found


def test_the_library_calls_what_it_is_given_but_a_method_of_the_project_is_left_alone(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """function f(x: number) { return x; }
function g() {}
class Box { map(fn: () => void) {} }
[1, 2].map(f);
setTimeout(g, 1);
function unrelated() {}
new Box().map(unrelated);
""",
    )
    assert ("main.ts", "main.f") in found
    assert ("main.ts", "main.g") in found
    assert ("main.ts", "main.unrelated") not in found
