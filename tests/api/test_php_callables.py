"""Which function a PHP call reaches when the callee is a value or a magic method.

A string, an array pair or `g(...)` handed to a function that calls its
parameter, a callable kept in a local or a property, `__call` and `__callStatic`,
and the names `$m` and `$class` hold when nothing else is assigned to them.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import RelationKind


def calls(tmp_path: Path, source: str) -> set[tuple[str, str]]:
    (tmp_path / "app.php").write_text("<?php\nnamespace App;\n" + source)
    graph = callix.PhpAdapter().analyze(str(tmp_path))
    return {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.CALLS
    }


def test_a_function_name_handed_to_a_function_that_calls_its_parameter(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """
function cb() {}
function run(callable $f) { $f(); }
run('App\\\\cb');
""",
    )
    assert ("App\\run", "App\\cb") in found


def test_a_string_is_not_a_call_when_the_function_does_not_call_it(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """
function cb() {}
function log_it($message) { echo $message; }
log_it('App\\\\cb');
""",
    )
    assert ("App\\log_it", "App\\cb") not in found
    assert not any(target == "App\\cb" for _, target in found)


def test_a_named_argument_reaches_the_parameter_of_that_name(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """
function one() {}
function two() {}
function run(callable $first, callable $second) { $second(); }
run(second: 'App\\\\two', first: 'App\\\\one');
""",
    )
    assert ("App\\run", "App\\two") in found
    assert ("App\\run", "App\\one") not in found


def test_a_first_class_callable_is_not_a_call_of_its_function(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """
function g() {}
function run(callable $f) { $f(); }
run(g(...));
""",
    )
    assert ("App\\run", "App\\g") in found
    assert ("app.php", "App\\g") not in found


def test_a_parameter_handed_on_reaches_the_function_that_calls_it(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """
function leaf() {}
function inner(callable $f) { $f(); }
function outer(callable $g) { inner($g); }
outer('App\\\\leaf');
""",
    )
    assert ("App\\inner", "App\\leaf") in found


def test_a_callable_kept_in_a_property_is_called_from_another_method(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """
function cb() {}
class Worker {
    private $f;
    public function __construct(callable $f) { $this->f = $f; }
    public function run() { ($this->f)(); }
}
(new Worker('App\\\\cb'))->run();
""",
    )
    assert ("App\\Worker\\run", "App\\cb") in found


def test_a_static_method_named_by_a_string(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """
class Util { public static function go() {} }
function run(callable $f) { $f(); }
run('App\\\\Util::go');
""",
    )
    assert ("App\\run", "App\\Util\\go") in found


def test_an_undeclared_method_goes_to_call_and_a_declared_one_does_not(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """
class Proxy {
    public function real() {}
    public function __call($name, $args) {}
    public static function __callStatic($name, $args) {}
}
$p = new Proxy();
$p->real();
$p->anything();
Proxy::other();
""",
    )
    assert ("app.php", "App\\Proxy\\real") in found
    assert ("app.php", "App\\Proxy\\__call") in found
    assert ("app.php", "App\\Proxy\\__callStatic") in found


def test_a_class_with_a_base_outside_the_project_may_inherit_the_method(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """
class Proxy extends \\Vendor\\Base {
    public function __call($name, $args) {}
}
(new Proxy())->inherited();
""",
    )
    assert not any(target == "App\\Proxy\\__call" for _, target in found)


def test_names_held_in_locals(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """
class Made { public function __construct() {} }
class A {
    public function go() {}
    public function run() { $m = 'go'; $this->$m(); }
}
function build() {
    $class = Made::class;
    return new $class();
}
""",
    )
    assert ("App\\A\\run", "App\\A\\go") in found
    assert ("App\\build", "App\\Made\\__construct") in found
