"""A `.h` with C++ syntax belongs to the C++ adapter (Callix-Tools/callix#49)."""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind


def write(root: Path, files: dict[str, str]) -> None:
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)


def file_paths(graph: callix.Graph) -> set[str]:
    return {n.file_path for n in graph.nodes.values() if n.kind is NodeKind.FILE}


def test_namespace_wrapped_h_header_is_analysed(tmp_path: Path) -> None:
    write(
        tmp_path,
        {
            "ns.h": "namespace a {\ninline int f(int x) { return x; }\n}\n",
            "m.cc": '#include "ns.h"\nint g() { return a::f(1); }\n',
        },
    )
    graph = callix.CppAdapter().analyze(str(tmp_path))
    assert "ns.h" in file_paths(graph)
    funcs = [n for n in graph.nodes.values() if n.kind is NodeKind.FUNCTION and n.qualified_name == "a::f(int)"]
    assert len(funcs) == 1 and funcs[0].file_path == "ns.h"
    calls = [
        r
        for r in graph.relations
        if r.kind is RelationKind.CALLS
        and graph.nodes[r.source_id].qualified_name.startswith("g(")
        and graph.nodes[r.target_id].qualified_name == "a::f(int)"
    ]
    assert len(calls) == 1
    assert "span" in calls[0].metadata
    assert calls[0].metadata["resolution"] == "name"
    ns_file = next(n for n in graph.nodes.values() if n.kind is NodeKind.FILE and n.file_path == "ns.h")
    assert any(
        r.kind is RelationKind.RESOLVES_TO and r.target_id == ns_file.id and graph.nodes[r.source_id].kind is NodeKind.IMPORT
        for r in graph.relations
    )
    assert not [n for n in graph.nodes.values() if n.kind is NodeKind.EXTERNAL_SYMBOL and n.qualified_name == "ns.h"]


def test_class_only_h_header_is_analysed(tmp_path: Path) -> None:
    write(
        tmp_path,
        {"x.h": "class C { int y; };\n", "u.cc": '#include "x.h"\nint u() { return 0; }\n'},
    )
    graph = callix.CppAdapter().analyze(str(tmp_path))
    assert "x.h" in file_paths(graph)
    assert [n for n in graph.nodes.values() if n.kind is NodeKind.CLASS and n.qualified_name == "C" and n.file_path == "x.h"]
    assert [n for n in graph.nodes.values() if n.kind is NodeKind.ATTRIBUTE and n.qualified_name == "C::y"]


NEUTRAL = {
    "cfg.h": "#define N 1\nint limit(void);\n",
    "m.cc": '#include "cfg.h"\nint limit(void) { return N; }\n',
}


def test_neutral_h_in_cpp_only_tree_belongs_to_cpp(tmp_path: Path) -> None:
    write(tmp_path, NEUTRAL)
    graph = callix.CppAdapter().analyze(str(tmp_path))
    assert "cfg.h" in file_paths(graph)
    limit = [n for n in graph.nodes.values() if n.kind is NodeKind.FUNCTION and n.qualified_name.startswith("limit")]
    assert limit
    assert any(site["file"] == "cfg.h" for n in limit for site in n.metadata["declared_at"])
    cfg = next(n for n in graph.nodes.values() if n.kind is NodeKind.FILE and n.file_path == "cfg.h")
    assert any(r.kind is RelationKind.RESOLVES_TO and r.target_id == cfg.id for r in graph.relations)


def test_neutral_h_with_c_sources_has_one_owner(tmp_path: Path) -> None:
    write(tmp_path, {**NEUTRAL, "x.c": "int x(void) { return 0; }\n"})
    c_graph = callix.CAdapter().analyze(str(tmp_path))
    cpp_graph = callix.CppAdapter().analyze(str(tmp_path))
    assert "cfg.h" in file_paths(c_graph)
    assert "cfg.h" not in file_paths(cpp_graph)
    c_graph.merge(cpp_graph, allow_shared=True)  # identical PROJECT/MODULE roots; must not raise
