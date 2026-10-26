"""What an edge says about how it was found.

A resolver's answer is one of several quite different things - a definition, the
variable a callee was read from, a symbol outside the project, a place callix
could not bind, a match by name - and the graph used to store all of them as
"resolved". Each edge now records `resolution`, and a call that landed on a value
is followed to what the value holds, marked `flow`.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind


def edges(graph: callix.Graph, kind: RelationKind = RelationKind.CALLS):
    for r in graph.relations:
        if r.kind is kind:
            yield (
                graph.nodes[r.source_id].qualified_name,
                graph.nodes[r.target_id].qualified_name,
                r.metadata.get("resolution"),
                r.metadata.get("via"),
            )


def python_graph(tmp_path: Path, source: str) -> callix.Graph:
    (tmp_path / "app.py").write_text(source)
    return callix.PythonAdapter().analyze(str(tmp_path))


def test_every_call_edge_says_how_it_was_resolved(tmp_path: Path) -> None:
    graph = python_graph(
        tmp_path,
        "import os\n\n\ndef f():\n    return 1\n\n\ndef main():\n    f()\n    os.getcwd()\n",
    )
    by_target = {t: how for _, t, how, _ in edges(graph)}
    assert by_target["app.f"] == "definition"
    # The standard library is outside the project: external, not a definition.
    assert "external" in by_target.values()
    assert all(how is not None for how in by_target.values())


def test_a_call_that_stops_at_a_variable_is_a_value_not_a_definition(tmp_path: Path) -> None:
    graph = python_graph(tmp_path, "def f(): ...\n\n\ndef main(g):\n    g()\n")
    assert ("app.main", "app.main.g", "value", None) in set(edges(graph))
    metrics = graph.metadata["resolver_metrics"]
    assert metrics["to_value"] == 1


def test_a_container_that_holds_a_function_is_followed_to_it(tmp_path: Path) -> None:
    graph = python_graph(tmp_path, 'def f(): ...\n\n\ntable = {"k": f}\ntable["k"]()\n')
    found = set(edges(graph))
    assert ("app.py", "app.table", "value", None) in found
    # `table["k"]()` may be a call of f, and the edge says it was followed, through what.
    assert ("app.py", "app.f", "flow", "app.table") in found


def test_a_callback_is_followed_from_the_call_site_to_the_parameter_that_calls_it(
    tmp_path: Path,
) -> None:
    graph = python_graph(
        tmp_path,
        "def cb(): ...\n\n\ndef run(f):\n    f()\n\n\nrun(cb)\nrun(f=cb)\n",
    )
    assert ("app.run", "app.cb", "flow", "app.run.f") in set(edges(graph))


def test_what_a_function_returns_is_followed(tmp_path: Path) -> None:
    graph = python_graph(
        tmp_path,
        "def g(): ...\n\n\ndef make():\n    return g\n\n\nmake()()\nh = make()\nh()\n",
    )
    flow = {(s, t) for s, t, how, _ in edges(graph) if how == "flow"}
    assert ("app.py", "app.g") in flow


def test_a_decorator_is_called_by_the_scope_the_definition_sits_in(tmp_path: Path) -> None:
    graph = python_graph(tmp_path, "def dec(f):\n    return f\n\n\n@dec\ndef g(): ...\n")
    assert ("app.py", "app.dec", "definition", None) in set(edges(graph))
    # The decorated definition still depends on what decorates it.
    refs = {(s, t) for s, t, _, _ in edges(graph, RelationKind.REFERENCES)}
    assert ("app.g", "app.dec") in refs


def test_raising_a_class_instantiates_it(tmp_path: Path) -> None:
    graph = python_graph(tmp_path, "class E(Exception):\n    pass\n\n\nraise E\n")
    assert ("app.py", "app.E", "definition", None) in set(edges(graph))


def test_the_analysed_directory_is_the_project_not_an_ancestors(tmp_path: Path) -> None:
    """ty walks up to the nearest pyproject.toml; a directory without one of its own
    used to be resolved from whatever surrounded it, and its absolute imports missed."""
    (tmp_path / "pyproject.toml").write_text('[project]\nname = "outer"\nversion = "0"\n')
    inner = tmp_path / "service"
    (inner / "pkg").mkdir(parents=True)
    (inner / "pkg" / "__init__.py").write_text("")
    (inner / "pkg" / "mod.py").write_text("def helper():\n    return 1\n")
    (inner / "main.py").write_text("from pkg.mod import helper\n\n\nhelper()\n")
    graph = callix.PythonAdapter().analyze(str(inner))
    assert ("main.py", "pkg.mod.helper", "definition", None) in set(edges(graph))


def test_typescript_new_super_and_overloads_are_calls_of_what_they_name(tmp_path: Path) -> None:
    (tmp_path / "a.ts").write_text(
        "class A { constructor() {} }\n"
        "class B extends A { constructor() { super(); } }\n"
        "function f(x: number): void;\n"
        "function f(x: string): void;\n"
        "function f(x: any) {}\n"
        "new B();\n"
        "f(1);\n"
    )
    graph = callix.TypeScriptAdapter().analyze(str(tmp_path))
    found = {(s, t) for s, t, _, _ in edges(graph)}
    assert ("a.ts", "a.B") in found  # new B()
    assert ("a.B.constructor", "a.A") in found  # super()
    # A call resolved to an overload signature lands on the implementation.
    assert ("a.ts", "a.f") in found


def test_a_closure_and_an_arrow_expression_body_belong_to_their_container(tmp_path: Path) -> None:
    (tmp_path / "a.ts").write_text(
        "function g() {}\n"
        "const arrow = () => g();\n"
        "function make() { return () => g(); }\n"
    )
    graph = callix.TypeScriptAdapter().analyze(str(tmp_path))
    found = {(s, t) for s, t, _, _ in edges(graph)}
    assert ("a.arrow", "a.g") in found
    assert ("a.make", "a.g") in found


def test_cpp_binds_a_member_call_by_the_receivers_declared_type(tmp_path: Path) -> None:
    (tmp_path / "main.cpp").write_text(
        "struct A { void run() {} };\n"
        "struct B { void run() {} };\n"
        "void f(A& a, B& b) { a.run(); b.run(); }\n"
        "struct Shape { virtual int area() { return 0; } };\n"
        "struct Sq : Shape { int area() override { return 1; } };\n"
        "int use(Shape& s) { return s.area(); }\n"
        "int direct() { Sq q; return q.area(); }\n"
    )
    graph = callix.CppAdapter().analyze(str(tmp_path))
    found = {(s, t) for s, t, _, _ in edges(graph)}
    assert ("f(A&,B&)", "A::run()") in found
    assert ("f(A&,B&)", "B::run()") in found
    assert ("use(Shape&)", "Shape::area()") in found
    assert ("direct()", "Sq::area()") in found
    # A table of names does not claim more: every one of those edges says so.
    assert all(how == "name" for *_, how, _ in [e for e in edges(graph) if e[1].endswith("run()")])


def test_cpp_says_when_a_name_is_not_unique_and_the_receiver_is_unknown(tmp_path: Path) -> None:
    (tmp_path / "main.cpp").write_text(
        "struct A { void run() {} };\n"
        "struct B { void run() {} };\n"
        "template <typename T> void f(T& t) { t.run(); }\n"
    )
    graph = callix.CppAdapter().analyze(str(tmp_path))
    flagged = [
        r
        for r in graph.relations
        if r.kind is RelationKind.CALLS and r.metadata.get("ambiguous") is True
    ]
    assert flagged, "an edge chosen between two declarations of one name must say so"


def test_cpp_picks_the_overload_the_arguments_fit(tmp_path: Path) -> None:
    (tmp_path / "main.cpp").write_text(
        "void f(int) {}\nvoid f(double) {}\nvoid g() { f(1); f(1.0); }\n"
    )
    graph = callix.CppAdapter().analyze(str(tmp_path))
    found = {t for s, t, _, _ in edges(graph) if s == "g()"}
    assert found == {"f(int)", "f(double)"}


def test_a_constructor_call_is_a_call_of_the_constructor(tmp_path: Path) -> None:
    (tmp_path / "main.cpp").write_text(
        "struct A { A() {} A(int) {} };\n"
        "struct B : A { B() : A() {} };\n"
        "void f() { A a; A b(1); B c; }\n"
    )
    graph = callix.CppAdapter().analyze(str(tmp_path))
    found = {(s, t) for s, t, _, _ in edges(graph)}
    assert ("f()", "A::A()") in found
    assert ("f()", "A::A(int)") in found
    assert ("f()", "B::B()") in found
    assert ("B::B()", "A::A()") in found


def test_a_go_call_through_an_interface_lands_on_the_interfaces_method(tmp_path: Path) -> None:
    (tmp_path / "go.mod").write_text("module m\n\ngo 1.21\n")
    (tmp_path / "main.go").write_text(
        "package main\n\n"
        "type I interface{ M() }\n\n"
        "func use(i I) { i.M() }\n\n"
        "func main() {}\n"
    )
    graph = callix.GoAdapter().analyze(str(tmp_path))
    if graph.metadata["resolver_status"] == "unavailable":
        return  # no Go toolchain here
    assert ("m.use", "m.I.M", "definition", None) in set(edges(graph))
    assert any(
        n.kind is NodeKind.METHOD and n.qualified_name == "m.I.M" for n in graph.nodes.values()
    )


def test_a_function_held_in_a_go_variable_is_followed(tmp_path: Path) -> None:
    (tmp_path / "go.mod").write_text("module m\n\ngo 1.21\n")
    (tmp_path / "main.go").write_text(
        "package main\n\n"
        "func g() {}\n\n"
        "func run(f func()) { f() }\n\n"
        "func main() {\n\th := g\n\th()\n\trun(g)\n}\n"
    )
    graph = callix.GoAdapter().analyze(str(tmp_path))
    if graph.metadata["resolver_status"] == "unavailable":
        return
    flow = {(s, t) for s, t, how, _ in edges(graph) if how == "flow"}
    assert ("m.main", "m.g") in flow
    assert ("m.run", "m.g") in flow
