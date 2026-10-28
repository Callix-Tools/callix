"""PHP language constructs and magic constants are syntax, not symbols.

`isset`, `empty`, `eval`, `die` and the magic constants must not become
EXTERNAL_SYMBOL nodes with CALLS/REFERENCES edges, while the calls inside their
arguments still produce edges.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

SYNTAX = {
    "isset", "empty", "eval", "exit", "die", "unset", "list", "array", "print", "echo",
    "include", "include_once", "require", "require_once",
    "__line__", "__file__", "__dir__", "__function__", "__class__", "__trait__",
    "__method__", "__namespace__", "__property__", "__compiler_halt_offset__",
}

SOURCE = """<?php
namespace App;

function is_set($x) { return $x; }
function key() { return 'k'; }
function load() { return 'return 1;'; }

function g($c, $o) {
    isset($c['k'], $c['j']);
    ISSET($c['x']);
    Empty($c);
    eval('return 1;');
    eval(load());
    die('x');
    exit(1);
    exit;
    unset($c['k']);
    list($p, $q) = $c;
    print 'x';
    print('y');
    echo 'z';
    include __DIR__ . '/a.php';
    include_once __DIR__ . '/a.php';
    require __DIR__ . '/a.php';
    require_once __DIR__ . '/a.php';
    $m = [__LINE__, __FILE__, __DIR__, __FUNCTION__, __CLASS__, __TRAIT__,
          __METHOD__, __NAMESPACE__, __PROPERTY__, __COMPILER_HALT_OFFSET__, __dir__];
    isset($o->m(key()));
    strlen('x');
    is_set(1);
}
"""


def test_constructs_and_magic_constants_leave_no_external_edges(tmp_path: Path) -> None:
    (tmp_path / "a.php").write_text(SOURCE)
    graph = callix.PhpAdapter().analyze(str(tmp_path))

    syntax_ids = {
        nid
        for nid, node in graph.nodes.items()
        if node.kind is NodeKind.EXTERNAL_SYMBOL and node.name.lower() in SYNTAX
    }
    assert not syntax_ids

    edges = [r for r in graph.relations if r.kind in (RelationKind.CALLS, RelationKind.REFERENCES)]
    assert not [r for r in edges if r.target_id in syntax_ids]

    calls = {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.CALLS
    }
    assert ("App\\g", "App\\is_set") in calls
    assert ("App\\g", "App\\key") in calls
    assert ("App\\g", "App\\load") in calls
    assert any(src == "App\\g" and tgt.endswith("strlen") for src, tgt in calls)

    for r in graph.relations:
        if r.kind is RelationKind.CALLS:
            assert "span" in r.metadata
