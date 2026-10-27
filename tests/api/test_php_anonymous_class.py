"""An anonymous class is a scope of its own for the PHP resolver.

Its `$this`, `self`, `static` and `parent` are the anonymous class and its base,
not the class that happens to contain the `new` expression, and its methods do
not shadow that class's members.
"""

from __future__ import annotations

import re
from pathlib import Path

import callix
from callix import RelationKind

ANON = re.compile(r"class@\d+:\d+")


def analyze(tmp_path: Path, source: str) -> callix.Graph:
    (tmp_path / "a.php").write_text(source)
    return callix.PhpAdapter().analyze(str(tmp_path))


def edges(graph: callix.Graph, kind: RelationKind) -> set[tuple[str, str]]:
    return {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is kind
    }


def resolutions(graph: callix.Graph, kind: RelationKind, source: str, target: str) -> set[str]:
    return {
        r.metadata.get("resolution")
        for r in graph.relations
        if r.kind is kind
        and graph.nodes[r.source_id].qualified_name == source
        and graph.nodes[r.target_id].qualified_name == target
    }


def anon_names(graph: callix.Graph) -> list[str]:
    # members of an anonymous class: a `class@L:C` segment followed by a member name
    return sorted(
        n.qualified_name
        for n in graph.nodes.values()
        if ANON.search(n.qualified_name) and not ANON.fullmatch(n.qualified_name.split("\\")[-1])
    )


BASE_HOST = """<?php
namespace App;

class Base {
    public $items = [];
    public function run() {}
    public function passes() {}
    public function m() {}
}

class Host {
    public $items = 1;
    public function make() {
        $o = new class extends Base {
            public function passes() { return parent::m(); }
            public function go() { $this->run(); $this->passes(); return $this->items; }
        };
        $o->go();
        $this->passes();
    }
    public function passes() {}
}
"""


def go_name(graph: callix.Graph) -> str:
    (name,) = [n for n in anon_names(graph) if n.endswith("\\go")]
    return name


def test_property_read_goes_to_the_base(tmp_path: Path) -> None:
    graph = analyze(tmp_path, BASE_HOST)
    refs = edges(graph, RelationKind.REFERENCES)
    assert (go_name(graph), "App\\Base\\items") in refs
    assert (go_name(graph), "App\\Host\\items") not in refs
    assert resolutions(graph, RelationKind.REFERENCES, go_name(graph), "App\\Base\\items") == {"definition"}


def test_this_call_goes_to_the_base(tmp_path: Path) -> None:
    graph = analyze(tmp_path, BASE_HOST)
    calls = edges(graph, RelationKind.CALLS)
    assert (go_name(graph), "App\\Base\\run") in calls
    assert (go_name(graph), "App\\Host\\run") not in calls
    assert resolutions(graph, RelationKind.CALLS, go_name(graph), "App\\Base\\run") == {"definition"}


def test_host_method_is_not_shadowed(tmp_path: Path) -> None:
    graph = analyze(tmp_path, BASE_HOST)
    calls = edges(graph, RelationKind.CALLS)
    assert ("App\\Host\\make", "App\\Host\\passes") in calls
    # the anonymous class's own `passes` is reached from its own `go`
    anon_passes = [n for n in anon_names(graph) if n.endswith("\\passes")]
    assert len(anon_passes) == 1
    assert (go_name(graph), anon_passes[0]) in calls
    assert ("App\\Host\\make", anon_passes[0]) not in calls


def test_parent_call_inside_the_anonymous_class(tmp_path: Path) -> None:
    graph = analyze(tmp_path, BASE_HOST)
    (anon_passes,) = [n for n in anon_names(graph) if n.endswith("\\passes")]
    assert (anon_passes, "App\\Base\\m") in edges(graph, RelationKind.CALLS)


def test_local_holding_an_anonymous_class(tmp_path: Path) -> None:
    graph = analyze(tmp_path, BASE_HOST)
    assert ("App\\Host\\make", go_name(graph)) in edges(graph, RelationKind.CALLS)


def test_nested_anonymous_class(tmp_path: Path) -> None:
    graph = analyze(
        tmp_path,
        """<?php
namespace App;

class Host {
    public function make() {
        return new class {
            public function go() {
                $i = new class {
                    public function x() { return 1; }
                    public function y() { return $this->x(); }
                };
                return $i->y();
            }
        };
    }
}
""",
    )
    names = anon_names(graph)
    inner_x = [n for n in names if n.endswith("\\x")]
    inner_y = [n for n in names if n.endswith("\\y")]
    assert len(inner_x) == 1 and len(inner_y) == 1
    assert re.fullmatch(r"App\\Host\\make\\class@\d+:\d+\\go\\class@\d+:\d+\\x", inner_x[0])
    assert (inner_y[0], inner_x[0]) in edges(graph, RelationKind.CALLS)


def test_anonymous_class_as_constructor_argument(tmp_path: Path) -> None:
    graph = analyze(
        tmp_path,
        """<?php
namespace App;

class Host {
    public function make() {
        return new class(new class {
            public function a() { return $this->b(); }
            public function b() { return 1; }
        }) {
            public function c() { return $this->d(); }
            public function d() { return 2; }
        };
    }
}
""",
    )
    calls = edges(graph, RelationKind.CALLS)
    by_leaf = {n.rsplit("\\", 1)[-1]: n for n in anon_names(graph)}
    # both classes are named under the outer prefix (the argument is not nested)
    for leaf in ("a", "b", "c", "d"):
        assert re.fullmatch(r"App\\Host\\make\\class@\d+:\d+\\" + leaf, by_leaf[leaf])
    assert (by_leaf["a"], by_leaf["b"]) in calls
    assert (by_leaf["c"], by_leaf["d"]) in calls


def test_new_static_targets_the_anonymous_class(tmp_path: Path) -> None:
    graph = analyze(
        tmp_path,
        """<?php
namespace App;

class Host {
    public function make() {
        return new class {
            public function again() { return new static(); }
        };
    }
}
""",
    )
    (again,) = [n for n in anon_names(graph) if n.endswith("\\again")]
    class_name = again.rsplit("\\", 1)[0]
    class_node = next(n for n in graph.nodes.values() if n.qualified_name == class_name)
    targets = [
        r
        for r in graph.relations
        if r.target_id == class_node.id and graph.nodes[r.source_id].qualified_name == again
    ]
    assert targets, "new static inside an anonymous class must reach the class node"
    assert not any(
        graph.nodes[r.target_id].qualified_name.startswith("App\\Host\\make\\class@") is False
        and graph.nodes[r.target_id].kind.name == "EXTERNAL_SYMBOL"
        and "static" in graph.nodes[r.target_id].qualified_name
        for r in graph.relations
        if graph.nodes[r.source_id].qualified_name == again
    )
