"""A conversion `T(x)` to a project type is a REFERENCES edge, never a CALLS edge."""

from __future__ import annotations

import shutil
from pathlib import Path

import callix
import pytest
from callix import RelationKind

needs_go = pytest.mark.skipif(shutil.which("go") is None, reason="the Go toolchain is not installed")

P = "example.com/m"


def analyse(tmp_path: Path, source: str, extra: dict[str, str] | None = None) -> callix.Graph:
    (tmp_path / "go.mod").write_text("module example.com/m\n\ngo 1.22\n")
    (tmp_path / "a.go").write_text(source)
    for name, text in (extra or {}).items():
        target = tmp_path / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
    return callix.GoAdapter().analyze(str(tmp_path))


def edges(graph: callix.Graph, kinds=(RelationKind.REFERENCES, RelationKind.CALLS)) -> list:
    """(source qname, target qname, kind, metadata, (line, col)) of every positioned edge."""
    out = []
    for r in graph.relations:
        span = r.metadata.get("span")
        if r.kind in kinds and span is not None:
            out.append(
                (
                    graph.nodes[r.source_id].qualified_name,
                    graph.nodes[r.target_id].qualified_name,
                    r.kind,
                    r.metadata,
                    (span.start_line, span.start_col),
                )
            )
    return out


ISSUE = """package m

type Accounts map[string]string

type Name string

func Use(m map[string]string) Name {
\ta := Accounts(m)
\t_ = a
\treturn Name("x")
}
"""


@needs_go
def test_conversion_to_project_type_is_a_reference(tmp_path: Path) -> None:
    graph = analyse(tmp_path, ISSUE)
    use = f"{P}.Use"
    expected = {(8, 7): f"{P}.Accounts", (10, 9): f"{P}.Name"}
    for pos, target in expected.items():
        found = [
            e
            for e in edges(graph, (RelationKind.REFERENCES,))
            if e[4] == pos and e[0] == use and e[1] == target
        ]
        assert len(found) == 1, (pos, target, found)
        assert found[0][3]["resolution"] == "definition"
        assert found[0][3]["access"] == "read"
    for source, target, kind, _, pos in edges(graph, (RelationKind.CALLS,)):
        assert target not in expected.values(), (source, target, pos)


@needs_go
def test_conversion_to_type_of_another_package(tmp_path: Path) -> None:
    source = """package m

import "example.com/m/sub"

func Use() {
\t_ = sub.ID(3)
}
"""
    graph = analyse(tmp_path, source, {"sub/s.go": "package sub\n\ntype ID int\n"})
    refs = [e for e in edges(graph, (RelationKind.REFERENCES,)) if e[0] == f"{P}.Use"]
    to_type = [e for e in refs if e[1] == f"{P}/sub.ID"]
    assert len(to_type) == 1, refs
    assert to_type[0][3]["resolution"] == "definition"
    assert [e for e in refs if e[1] == f"{P}/sub"], refs
    assert not edges(graph, (RelationKind.CALLS,))


@needs_go
def test_method_on_conversion_is_still_a_call(tmp_path: Path) -> None:
    source = """package m

type HandlerFunc func()

func (h HandlerFunc) Serve() {}

func g() {}

func main() {
\tHandlerFunc(g).Serve()
}
"""
    graph = analyse(tmp_path, source)
    calls = [e for e in edges(graph, (RelationKind.CALLS,)) if e[0] == f"{P}.main"]
    serve = [e for e in calls if e[1].endswith("HandlerFunc.Serve")]
    assert len(serve) == 1, calls
    assert serve[0][3]["resolution"] == "definition"
    assert not [e for e in calls if e[1] == f"{P}.HandlerFunc"]
    refs = edges(graph, (RelationKind.REFERENCES,))
    assert [e for e in refs if e[0] == f"{P}.main" and e[1] == f"{P}.HandlerFunc"], refs
