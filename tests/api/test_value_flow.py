"""Which function a call reaches when it is made through a value.

Every case is a program whose answer a reader can work out from Python's rules:
a decorator rebinds the name, a container holds what was put in it under the key
it was put there with, a parameter holds what its callers pass, and `map` calls
what it is given. The type checker answers none of these, the flow pass does.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import RelationKind


def calls(tmp_path: Path, source: str) -> set[tuple[str, str]]:
    (tmp_path / "app.py").write_text(source)
    graph = callix.PythonAdapter().analyze(str(tmp_path))
    return {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.CALLS
    }


def test_a_decorated_name_stands_for_what_the_decorator_returns(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def deco(f):
    def inner():
        return f()
    return inner

@deco
def target():
    pass

target()
""",
    )
    assert ("app.py", "app.deco.inner") in found
    assert ("app.deco.inner", "app.target") in found


def test_stacked_decorators_apply_from_the_bottom(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def dec1(f):
    def inner():
        return f()
    return inner

def dec2(f):
    def inner():
        return f()
    return inner

@dec1
@dec2
def func():
    pass

func()
""",
    )
    assert ("app.py", "app.dec1.inner") in found
    assert ("app.dec1.inner", "app.dec2.inner") in found
    assert ("app.dec2.inner", "app.func") in found
    # `func` is rebound: the module never calls the original itself
    assert ("app.py", "app.func") not in found


def test_a_method_is_found_on_the_class_the_receiver_holds(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
class A:
    def run(self):
        pass

class B:
    def run(self):
        pass

def use(thing):
    thing.run()

use(B())
""",
    )
    assert ("app.use", "app.B.run") in found


def test_a_dictionary_gives_back_what_was_stored_under_the_key(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def one():
    pass

def two():
    pass

table = {"a": one, "b": two}
table["a"]()
""",
    )
    assert ("app.py", "app.one") in found
    assert ("app.py", "app.two") not in found


def test_a_list_gives_back_what_was_stored_at_the_index(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def one():
    pass

def two():
    pass

handlers = [one, two]
handlers[1]()
""",
    )
    assert ("app.py", "app.two") in found
    assert ("app.py", "app.one") not in found


def test_append_and_item_assignment_store_into_the_container(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def one():
    pass

def two():
    pass

handlers = []
handlers.append(one)
by_name = {}
by_name["x"] = two
handlers[0]()
by_name["x"]()
""",
    )
    assert {("app.py", "app.one"), ("app.py", "app.two")} <= found


def test_unpacking_follows_the_position(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def one():
    pass

def two():
    pass

a, b = one, two
b()
""",
    )
    assert ("app.py", "app.two") in found
    assert ("app.py", "app.one") not in found


def test_a_parameter_default_is_what_the_parameter_holds(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def fallback():
    pass

def run(cb=fallback):
    cb()

run()
""",
    )
    assert ("app.run", "app.fallback") in found


def test_a_call_of_a_call_reaches_what_the_inner_call_returns(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def target():
    pass

def inner():
    return target

def outer():
    return inner

outer()()()
""",
    )
    assert ("app.py", "app.target") in found


def test_a_parameter_called_in_a_function_is_called_with_what_callers_pass(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def one():
    pass

def two():
    pass

def apply(cb):
    cb()

apply(one)
apply(two)
""",
    )
    assert {("app.apply", "app.one"), ("app.apply", "app.two")} <= found


def test_the_library_calls_the_function_it_is_given(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def square(x):
    return x

def key(x):
    return x

def make():
    def made():
        pass
    return made

list(map(square, [1, 2]))
sorted([2, 1], key=key)
for r in map(lambda i: make(), [1]):
    pass
res = map(make, [1])
for f in res:
    f()
""",
    )
    assert ("app.py", "app.square") in found
    assert ("app.py", "app.key") in found
    # what `map` hands out are the results of the function it was given
    assert ("app.py", "app.make.made") in found


def test_a_name_the_library_does_not_call_is_not_called(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def square(x):
    return x

len(square)
print(square)
""",
    )
    assert ("app.py", "app.square") not in found


def test_a_function_of_the_project_named_like_a_library_one_is_left_alone(tmp_path: Path) -> None:
    found = calls(
        tmp_path,
        """\
def map(items, fn):
    return items

def square(x):
    return x

map([1, 2], square)
""",
    )
    assert ("app.py", "app.map") in found
    # what `map` does with its argument is its own business
    assert ("app.py", "app.square") not in found
