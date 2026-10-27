"""`self`, `static` and `parent` name a class; they are never a symbol to qualify.

The resolver once namespace-qualified them into `App\\static`, `App\\self` and
`App\\static\\val`, and recorded nothing at the keyword of `self::m()`.
"""

from __future__ import annotations

import re
from pathlib import Path

import callix
from callix import NodeKind, RelationKind

A_PHP = """<?php
namespace App;

class A
{
    public static function make(): static
    {
        return new static();
    }

    public function me(): self
    {
        return $this;
    }

    public function run(): int
    {
        self::make();
        return (new static)->val();
    }

    public function val(): int
    {
        return 1;
    }
}
"""


def analyze(tmp_path: Path, source: str) -> callix.Graph:
    (tmp_path / "a.php").write_text(source)
    return callix.PhpAdapter().analyze(str(tmp_path))


def edges(graph: callix.Graph, kind: RelationKind) -> set[tuple[str, str]]:
    return {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is kind
    }


def spans_to(graph: callix.Graph, target: str) -> set[tuple[int, int, int, int]]:
    out = set()
    for r in graph.relations:
        span = r.metadata.get("span")
        if span is None or graph.nodes[r.target_id].qualified_name != target:
            continue
        out.add((span.start_line, span.start_col, span.end_line, span.end_col))
    return out


def keyword_nodes(graph: callix.Graph) -> list[str]:
    pattern = re.compile(r"\\(static|self|parent)(\\|$)", re.IGNORECASE)
    return [n.qualified_name for n in graph.nodes.values() if pattern.search(n.qualified_name)]


def test_no_keyword_symbols(tmp_path: Path) -> None:
    graph = analyze(tmp_path, A_PHP)
    assert keyword_nodes(graph) == []


def test_return_types_are_the_class(tmp_path: Path) -> None:
    graph = analyze(tmp_path, A_PHP)
    has_type = edges(graph, RelationKind.HAS_TYPE)
    assert ("App\\A\\make", "App\\A") in has_type
    assert ("App\\A\\me", "App\\A") in has_type


def test_new_static_receiver(tmp_path: Path) -> None:
    graph = analyze(tmp_path, A_PHP)
    assert ("App\\A\\run", "App\\A\\val") in edges(graph, RelationKind.CALLS)


def test_new_static_in_a_local_and_a_self_typed_parameter(tmp_path: Path) -> None:
    graph = analyze(
        tmp_path,
        """<?php
namespace App;

class A
{
    public function val() {}
    public function local() { $x = new static; $x->val(); }
    public function typed(?self $o) { $o->val(); }
}
""",
    )
    calls = edges(graph, RelationKind.CALLS)
    assert ("App\\A\\local", "App\\A\\val") in calls
    assert ("App\\A\\typed", "App\\A\\val") in calls
    assert ("App\\A\\typed\\o", "App\\A") in edges(graph, RelationKind.HAS_TYPE)
    assert keyword_nodes(graph) == []


def test_self_keyword_is_a_reference(tmp_path: Path) -> None:
    graph = analyze(tmp_path, A_PHP)
    assert ("App\\A\\run", "App\\A") in edges(graph, RelationKind.REFERENCES)
    assert (18, 9, 18, 13) in spans_to(graph, "App\\A")
    hits = [
        r
        for r in graph.relations
        if r.kind is RelationKind.REFERENCES
        and graph.nodes[r.target_id].qualified_name == "App\\A"
        and r.metadata["span"].start_line == 18
    ]
    assert len(hits) == 1
    assert hits[0].metadata["access"] == "read"
    assert hits[0].metadata["resolution"] == "definition"


def test_static_and_class_constant_keywords(tmp_path: Path) -> None:
    graph = analyze(
        tmp_path,
        """<?php
namespace App;

class A
{
    const C = 1;
    public static function make() {}
    public function m()
    {
        static::make();
        $a = self::C;
        return static::class;
    }
}
""",
    )
    spans = spans_to(graph, "App\\A")
    assert (10, 9, 10, 15) in spans  # static::make()
    assert (11, 14, 11, 18) in spans  # self::C
    assert (12, 16, 12, 22) in spans  # static::class
    assert keyword_nodes(graph) == []


def test_parent_keyword(tmp_path: Path) -> None:
    graph = analyze(
        tmp_path,
        """<?php
namespace App;

class A { public function val() {} }
class B extends A
{
    public function m() { parent::val(); }
    public function n(): parent { return $this; }
}
""",
    )
    assert ("App\\B\\m", "App\\A\\val") in edges(graph, RelationKind.CALLS)
    assert ("App\\B\\m", "App\\A") in edges(graph, RelationKind.REFERENCES)
    assert ("App\\B\\n", "App\\A") in edges(graph, RelationKind.HAS_TYPE)
    assert keyword_nodes(graph) == []


def test_parent_outside_the_project(tmp_path: Path) -> None:
    graph = analyze(
        tmp_path,
        """<?php
namespace App;

class B extends \\Vendor\\Base
{
    public function m() { parent::x(); }
}
""",
    )
    externals = {
        n.qualified_name for n in graph.nodes.values() if n.kind is NodeKind.EXTERNAL_SYMBOL
    }
    assert "Vendor\\Base" in externals
    assert keyword_nodes(graph) == []
    assert ("App\\B\\m", "Vendor\\Base") in edges(graph, RelationKind.REFERENCES)


def test_keyword_without_a_class_makes_nothing(tmp_path: Path) -> None:
    graph = analyze(
        tmp_path,
        """<?php
namespace App;

class NoBase { public function m() { return parent::x(); } }
function f(self $o) { return self::g(); }
""",
    )
    assert keyword_nodes(graph) == []
    targets = {t for _, t in edges(graph, RelationKind.REFERENCES)}
    assert not any(t.lower().endswith(("self", "parent", "static")) for t in targets)


def test_keywords_are_case_insensitive(tmp_path: Path) -> None:
    graph = analyze(
        tmp_path,
        """<?php
namespace App;

class A
{
    public static function make(): Static { return new Static(); }
    public function m() { return SELF::make(); }
}
""",
    )
    assert ("App\\A\\m", "App\\A\\make") in edges(graph, RelationKind.CALLS)
    assert ("App\\A\\make", "App\\A") in edges(graph, RelationKind.HAS_TYPE)
    assert keyword_nodes(graph) == []


def test_instanceof_self(tmp_path: Path) -> None:
    graph = analyze(
        tmp_path,
        """<?php
namespace App;

class A
{
    public function same($x) { return $x instanceof self; }
}
""",
    )
    assert ("App\\A\\same", "App\\A") in edges(graph, RelationKind.HAS_TYPE)
    assert keyword_nodes(graph) == []
