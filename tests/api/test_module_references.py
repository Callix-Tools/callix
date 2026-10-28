"""A module named in code is a node with a position.

MODULE nodes used to have no file and no span, so a resolver answer that is a
module (ty and typescript-go both answer a module with its file at 1:1) bound to
nothing and became a site-keyed external symbol the exporters dropped.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

PY_FILES = {
    "pkg/__init__.py": "",
    "pkg/util.py": "def helper():\n    return 1\n",
    "pkg/main.py": (
        "from pkg import util\n"
        "import pkg.util\n"
        "\n"
        "\n"
        "def run():\n"
        "    return util.helper()\n"
    ),
}


def write(root: Path, files: dict[str, str]) -> None:
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)


def modules(graph: callix.Graph) -> dict[str, callix.Node]:
    return {n.qualified_name: n for n in graph.nodes.values() if n.kind is NodeKind.MODULE}


def edges_at(graph: callix.Graph, line: int, col: int, kind: RelationKind | None = None):
    found = []
    for r in graph.relations:
        span = r.metadata.get("span")
        if span is None or (span.start_line, span.start_col) != (line, col):
            continue
        if kind is None or r.kind is kind:
            found.append(r)
    return found


def target_names(graph: callix.Graph, relations) -> set[str]:
    return {graph.nodes[r.target_id].qualified_name for r in relations}


def test_a_module_with_a_file_is_placed_at_the_start_of_it(tmp_path: Path) -> None:
    write(tmp_path, PY_FILES)
    graph = callix.PythonAdapter(resolve=False).analyze(str(tmp_path))
    mods = modules(graph)
    util = mods["pkg.util"]
    one = callix.Span(1, 1, 1, 1)
    assert util.file_path == "pkg/util.py"
    assert util.span == one
    assert util.metadata["name_span"] == one
    # A package is its __init__.py, even though `pkg/main.py` came first in the walk.
    assert mods["pkg"].file_path == "pkg/__init__.py"


def test_a_namespace_package_stays_unplaced(tmp_path: Path) -> None:
    write(tmp_path, {"ns/a.py": "def f(): ...\n", "main.py": "import ns.a\n"})
    graph = callix.PythonAdapter(resolve=False).analyze(str(tmp_path))
    mods = modules(graph)
    assert mods["ns"].file_path is None
    assert mods["ns"].span is None
    assert mods["ns.a"].file_path == "ns/a.py"


def test_module_uses_bind_to_the_module(tmp_path: Path) -> None:
    write(tmp_path, PY_FILES)
    graph = callix.PythonAdapter(resolve=True).analyze(str(tmp_path))
    assert modules(graph)["pkg.util"].file_path == "pkg/util.py"

    # `from pkg import util`: the name the import brings in (1:17), and `pkg` (1:6).
    imported = edges_at(graph, 1, 17, RelationKind.RESOLVES_TO)
    assert target_names(graph, imported) == {"pkg.util"}
    assert all(r.metadata["resolution"] == "definition" for r in imported)
    assert target_names(graph, edges_at(graph, 1, 6)) == {"pkg"}

    # `import pkg.util`: each segment is a read of its module (2:8 and 2:12).
    assert target_names(graph, edges_at(graph, 2, 8, RelationKind.REFERENCES)) == {"pkg"}
    assert target_names(graph, edges_at(graph, 2, 12, RelationKind.REFERENCES)) == {"pkg.util"}

    # `util.helper()` in run(): the receiver `util` (6:12).
    used = edges_at(graph, 6, 12, RelationKind.REFERENCES)
    assert target_names(graph, used) == {"pkg.util"}
    assert all(r.metadata["resolution"] == "definition" for r in used)
    assert all(graph.nodes[r.source_id].qualified_name == "pkg.main.run" for r in used)

    assert not [n for n in graph.nodes.values() if n.qualified_name == "read@pkg/main.py:6:12"]


def test_a_name_at_the_start_of_a_file_beats_its_module(tmp_path: Path) -> None:
    write(
        tmp_path,
        {
            "pkg/__init__.py": "",
            "pkg/consts.py": "x = 1\n",
            "pkg/main.py": "from pkg import consts\n\n\ndef f():\n    return consts.x\n",
        },
    )
    graph = callix.PythonAdapter(resolve=True).analyze(str(tmp_path))
    # `x` (5:19) sits at 1:1 of consts.py: the variable, not the module.
    hit = edges_at(graph, 5, 19, RelationKind.REFERENCES)
    assert target_names(graph, hit) == {"pkg.consts.x"}
    assert {graph.nodes[r.target_id].kind for r in hit} == {NodeKind.VARIABLE}


def test_an_external_import_adds_no_module_reads(tmp_path: Path) -> None:
    write(tmp_path, {"main.py": "import os.path\n"})
    graph = callix.PythonAdapter(resolve=True).analyze(str(tmp_path))
    assert edges_at(graph, 1, 8) == []
    assert edges_at(graph, 1, 11) == []


def test_a_typescript_namespace_import_binds_to_the_module(tmp_path: Path) -> None:
    write(
        tmp_path,
        {
            "a.ts": "export function foo(): number {\n  return 1;\n}\n",
            "b.ts": (
                'import * as a from "./a";\n'
                "\n"
                "export function run(): number {\n"
                "  return a.foo();\n"
                "}\n"
            ),
            "tsconfig.json": '{"compilerOptions":{"strict":true,"module":"esnext",'
            '"target":"es2020","moduleResolution":"bundler"}}',
        },
    )
    graph = callix.TypeScriptAdapter(resolve=True).analyze(str(tmp_path))
    mod_a = modules(graph)["a"]
    assert mod_a.file_path == "a.ts"
    assert mod_a.span == callix.Span(1, 1, 1, 1)

    hits = edges_at(graph, 4, 10, RelationKind.REFERENCES)
    assert [(graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].id) for r in hits] == [
        ("b.run", mod_a.id)
    ]
    assert all(r.metadata["resolution"] != "unlocated" for r in hits)
    assert hits[0].metadata["span"] == callix.Span(4, 10, 4, 11)
    assert not [n for n in graph.nodes.values() if n.qualified_name == "read@b.ts:4:10"]
