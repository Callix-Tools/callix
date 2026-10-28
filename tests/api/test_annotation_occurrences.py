"""Every name in a Python annotation is a use of that name.

The visitor once recorded only the first identifier of a parameter or return
annotation, so `list[Item]`, `Optional["Item"]`, `dict[str, Item] | None` and every
annotated variable or class field produced no HAS_TYPE edge to `Item`.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

ITEM = "pkg.models.Item"

USE = '''\
from typing import Optional
from pkg.models import Item


class Box:
    items: list[Item] = []


def f(a: list[Item], b: Optional["Item"], c: dict[str, Item] | None) -> list[Item]:
    x: list[Item] = []
    return x
'''


def _project(root: Path, use: str) -> callix.Graph:
    pkg = root / "pkg"
    pkg.mkdir()
    (pkg / "__init__.py").write_text("")
    (pkg / "models.py").write_text("class Item:\n    pass\n")
    (pkg / "use.py").write_text(use)
    return callix.PythonAdapter().analyze(str(root))


def _edges(graph: callix.Graph, kind: RelationKind) -> list[tuple[str, str, int, int, str]]:
    """(source qualified name, target qualified name, line, col, resolution)."""
    found = []
    for r in graph.relations:
        span = r.metadata.get("span")
        if r.kind is not kind or not isinstance(span, callix.Span):
            continue
        found.append(
            (
                graph.nodes[r.source_id].qualified_name,
                graph.nodes[r.target_id].qualified_name,
                span.start_line,
                span.start_col,
                r.metadata.get("resolution"),
            )
        )
    return found


def _col(use: str, line: int, needle: str, nth: int = 0) -> int:
    text = use.split("\n")[line - 1]
    index = -1
    for _ in range(nth + 1):
        index = text.index(needle, index + 1)
    return index + 1


def test_annotation_names_in_compound_string_and_variable_annotations(tmp_path: Path) -> None:
    graph = _project(tmp_path, USE)
    got = {(line, col): (src, res) for src, tgt, line, col, res in _edges(graph, RelationKind.HAS_TYPE) if tgt == ITEM}
    assert got == {
        (6, 17): ("pkg.use.Box.items", "definition"),
        (9, 15): ("pkg.use.f.a", "definition"),
        (9, 35): ("pkg.use.f.b", "definition"),
        (9, 56): ("pkg.use.f.c", "definition"),
        (9, 78): ("pkg.use.f", "definition"),
        (10, 13): ("pkg.use.f.x", "definition"),
    }


def test_annotation_only_declaration_is_a_type_not_a_value(tmp_path: Path) -> None:
    use = "from pkg.models import Item\n\n\nclass Box:\n    n: Item\n\n\ndef g():\n    y: Item\n"
    graph = _project(tmp_path, use)
    has_type = {(src, line, col) for src, tgt, line, col, _ in _edges(graph, RelationKind.HAS_TYPE) if tgt == ITEM}
    assert ("pkg.use.Box.n", 5, 8) in has_type
    assert ("pkg.use.g.y", 9, 8) in has_type
    refs = {(src, line, col) for src, tgt, line, col, _ in _edges(graph, RelationKind.REFERENCES) if tgt == ITEM}
    assert ("pkg.use.Box", 5, 8) not in refs
    assert ("pkg.use.g", 9, 8) not in refs


def test_qualified_annotation_targets_the_class(tmp_path: Path) -> None:
    use = "from pkg import models\n\n\ndef g():\n    d: models.Item\n"
    graph = _project(tmp_path, use)
    has_type = _edges(graph, RelationKind.HAS_TYPE)
    assert ("pkg.use.g.d", ITEM, 5, 15, "definition") in has_type
    # `models` is a read of the module, never the type.
    refs = _edges(graph, RelationKind.REFERENCES)
    assert any(src == "pkg.use.g" and tgt == "pkg.models" and (line, col) == (5, 8) for src, tgt, line, col, _ in refs)
    module_ids = {n.id for n in graph.nodes.values() if n.kind is NodeKind.MODULE}
    for r in graph.relations:
        span = r.metadata.get("span")
        if r.kind is RelationKind.HAS_TYPE and isinstance(span, callix.Span) and span.start_line == 5:
            assert r.target_id not in module_ids


def test_literal_and_annotated_metadata_are_not_types(tmp_path: Path) -> None:
    use = (
        "from typing import Annotated, Literal\n"
        "from pkg.models import Item\n\n\n"
        "def dep():\n    return 1\n\n\n"
        'def f(a: Literal["Item"], b: Annotated[Item, dep()]):\n    pass\n'
    )
    graph = _project(tmp_path, use)
    line = 9
    string_col = _col(use, line, '"Item"') + 1
    item_col = _col(use, line, "Annotated[Item") + len("Annotated[")
    at_item = [
        (kind, line_, col)
        for kind in (RelationKind.HAS_TYPE, RelationKind.REFERENCES)
        for _, tgt, line_, col, _ in _edges(graph, kind)
        if tgt == ITEM
    ]
    assert (RelationKind.HAS_TYPE, line, item_col) in at_item
    assert not any(col == string_col for _, _, col in at_item)
    assert not any(
        tgt == "pkg.use.dep" for _, tgt, *_ in _edges(graph, RelationKind.HAS_TYPE)
    )
    reached = [
        (src, tgt)
        for kind in (RelationKind.CALLS, RelationKind.REFERENCES)
        for src, tgt, *_ in _edges(graph, kind)
    ]
    assert ("pkg.use.f", "pkg.use.dep") in reached


def test_unparseable_string_annotation_is_ignored(tmp_path: Path) -> None:
    use = 'X = 1\na: "not valid("\nb: f"{X}"\n'
    graph = _project(tmp_path, use)
    lines = {line for _, _, line, _, _ in _edges(graph, RelationKind.HAS_TYPE)}
    assert 2 not in lines
    assert 3 not in lines
