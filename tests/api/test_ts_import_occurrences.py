"""TypeScript import and export specifiers and module strings are placed."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

import callix
from callix import NodeKind, RelationKind

REPO = Path(__file__).resolve().parents[2]
TSCONFIG = (
    '{"compilerOptions":{"strict":true,"module":"esnext","moduleResolution":"bundler"},'
    '"include":["*.ts"]}'
)
A_TS = (
    "export interface Foo { x: number }\n"
    "export const bar = 1;\n"
    "export default function dflt() {}\n"
)
B_TS = (
    'import type { Foo } from "./a";\n'
    'import { bar } from "./a";\n'
    "const local = bar;\n"
    "export { local };\n"
    "export const f: Foo = { x: local };\n"
)


def write(root: Path, b_ts: str, a_ts: str = A_TS) -> None:
    (root / "a.ts").write_text(a_ts)
    (root / "b.ts").write_text(b_ts)
    (root / "tsconfig.json").write_text(TSCONFIG)


def analyze(root: Path, resolve: bool = True) -> callix.Graph:
    return callix.TypeScriptAdapter(resolve=resolve).analyze(str(root))


def spans(graph: callix.Graph, kind: RelationKind | None = None):
    out = []
    for r in graph.relations:
        span = r.metadata.get("span")
        if span is None or (kind is not None and r.kind is not kind):
            continue
        out.append(
            (
                r,
                (span.start_line, span.start_col, span.end_line, span.end_col),
            )
        )
    return out


def edge(graph, kind, span, source=None):
    """Relations of `kind` whose span equals `span` (4-tuple), optionally from `source` qname."""
    found = []
    for r, s in spans(graph, kind):
        if s != span:
            continue
        if source is not None and graph.nodes[r.source_id].qualified_name != source:
            continue
        found.append(r)
    return found


def node(graph, qname: str, kind: NodeKind):
    return next(
        n for n in graph.nodes.values() if n.qualified_name == qname and n.kind is kind
    )


def starting_at(graph, kind, line, col):
    return [r for r, s in spans(graph, kind) if (s[0], s[1]) == (line, col)]


def test_import_and_export_specifiers_are_placed(tmp_path: Path) -> None:
    write(tmp_path, B_TS)
    graph = analyze(tmp_path)
    mod_a = node(graph, "a", NodeKind.MODULE)

    foo = edge(graph, RelationKind.RESOLVES_TO, (1, 15, 1, 18), "b.Foo")
    assert [graph.nodes[r.target_id].qualified_name for r in foo] == ["a.Foo"]
    assert foo[0].metadata["resolution"] == "definition"

    bar = edge(graph, RelationKind.RESOLVES_TO, (2, 10, 2, 13), "b.bar")
    assert [graph.nodes[r.target_id].qualified_name for r in bar] == ["a.bar"]
    assert bar[0].metadata["resolution"] == "definition"

    for line, col in ((1, 26), (2, 21)):
        hits = starting_at(graph, RelationKind.REFERENCES, line, col)
        assert [r.target_id for r in hits] == [mod_a.id]
        assert hits[0].metadata["access"] == "read"
        assert hits[0].metadata["resolution"] == "definition"
        span = hits[0].metadata["span"]
        assert span.end_col - span.start_col == len('"./a"')

    local = starting_at(graph, RelationKind.REFERENCES, 4, 10)
    assert [graph.nodes[r.target_id].qualified_name for r in local] == ["b.local"]
    assert local[0].metadata["access"] == "read"

    assert not [
        n
        for n in graph.nodes.values()
        if n.kind is NodeKind.EXTERNAL_SYMBOL and n.name.startswith(("read@", "import@"))
    ]

    imp = node(graph, "b.Foo", NodeKind.IMPORT)
    ns = imp.metadata["name_span"]
    assert (ns.start_line, ns.start_col, ns.end_line, ns.end_col) == (1, 15, 1, 18)
    assert imp.span is None


def test_aliased_default_and_namespace_imports(tmp_path: Path) -> None:
    b = (
        'import { bar as baz } from "./a";\n'
        'import d from "./a";\n'
        'import * as NS from "./a";\n'
        "export const all = [baz, d, NS];\n"
    )
    write(tmp_path, b)
    graph = analyze(tmp_path)

    # `bar` (the original), not `baz`, is the position.
    hit = edge(graph, RelationKind.RESOLVES_TO, (1, 10, 1, 13), "b.baz")
    assert [graph.nodes[r.target_id].qualified_name for r in hit] == ["a.bar"]
    assert edge(graph, RelationKind.RESOLVES_TO, (1, 17, 1, 20)) == []
    ns = node(graph, "b.baz", NodeKind.IMPORT).metadata["name_span"]
    assert (ns.start_line, ns.start_col, ns.end_line, ns.end_col) == (1, 17, 1, 20)

    d = edge(graph, RelationKind.RESOLVES_TO, (2, 8, 2, 9), "b.d")
    assert [(graph.nodes[r.target_id].kind, graph.nodes[r.target_id].qualified_name) for r in d] == [
        (NodeKind.FUNCTION, "a.dflt")
    ]

    star = edge(graph, RelationKind.RESOLVES_TO, (3, 13, 3, 15), "b.NS")
    assert [graph.nodes[r.target_id].qualified_name for r in star] == ["a"]
    assert graph.nodes[star[0].target_id].kind is NodeKind.MODULE


def test_reexports_are_imports_and_placed(tmp_path: Path) -> None:
    b = (
        'export * from "./a";\n'
        'export * as R from "./a";\n'
        'export { bar as qux } from "./a";\n'
    )
    write(tmp_path, b)
    graph = analyze(tmp_path)
    mod_a = node(graph, "a", NodeKind.MODULE)
    node(graph, "b.__reexport___a", NodeKind.IMPORT)

    for line, col in ((1, 15), (2, 20), (3, 28)):
        hits = starting_at(graph, RelationKind.REFERENCES, line, col)
        assert [r.target_id for r in hits] == [mod_a.id]

    bar = starting_at(graph, RelationKind.REFERENCES, 3, 10)
    assert [graph.nodes[r.target_id].qualified_name for r in bar] == ["a.bar"]
    assert starting_at(graph, RelationKind.REFERENCES, 3, 17) == []
    assert starting_at(graph, RelationKind.RESOLVES_TO, 3, 17) == []

    file_b = next(
        n for n in graph.nodes.values() if n.kind is NodeKind.FILE and n.file_path == "b.ts"
    )
    bare = [
        r
        for r in graph.relations
        if r.kind is RelationKind.IMPORTS
        and r.source_id == file_b.id
        and r.target_id == mod_a.id
        and not r.metadata.get("span")
    ]
    assert len(bare) == 1


def test_external_module_strings_add_nothing(tmp_path: Path) -> None:
    write(tmp_path, 'import { x } from "some-pkg";\nexport const y = x;\n')
    graph = analyze(tmp_path)
    line1 = [r for r, s in spans(graph) if s[0] == 1 and r.kind in (RelationKind.REFERENCES, RelationKind.RESOLVES_TO)]
    assert line1 == []
    assert not [
        n
        for n in graph.nodes.values()
        if n.kind is NodeKind.EXTERNAL_SYMBOL and n.name.startswith(("read@", "import@"))
    ]


def test_path_alias_module_string_adds_no_external_symbol(tmp_path: Path) -> None:
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "store.ts").write_text("export const bar = 1;\n")
    (tmp_path / "src" / "use.ts").write_text(
        'import { bar } from "@/store";\nexport const y = bar;\n'
    )
    (tmp_path / "tsconfig.json").write_text(
        '{"compilerOptions":{"strict":true,"module":"esnext","moduleResolution":"bundler",'
        '"baseUrl":".","paths":{"@/*":["src/*"]}},"include":["src"]}'
    )
    graph = analyze(tmp_path)
    # No site-keyed external for the aliased module string.
    assert not [
        n
        for n in graph.nodes.values()
        if n.kind is NodeKind.EXTERNAL_SYMBOL and n.name.startswith(("read@", "import@"))
    ]
    # The module string (line 1, col 21) carries no REFERENCES edge.
    assert not [
        r
        for r, s in spans(graph, RelationKind.REFERENCES)
        if (s[0], s[1]) == (1, 21)
    ]
    # The specifier `bar` (line 1, col 10) is still placed.
    assert starting_at(graph, RelationKind.RESOLVES_TO, 1, 10)


def test_resolve_false_is_structural_only(tmp_path: Path) -> None:
    write(tmp_path, B_TS)
    graph = analyze(tmp_path, resolve=False)
    imp = node(graph, "b.Foo", NodeKind.IMPORT)
    assert "name_span" in imp.metadata
    assert [r for r, _ in spans(graph, RelationKind.RESOLVES_TO)] == []
    assert [r for r, _ in spans(graph, RelationKind.REFERENCES)] == []


def _binary() -> Path | None:
    override = os.environ.get("CALLIX_CLI")
    if override:
        return Path(override)
    for profile in ("release", "debug"):
        candidate = REPO / "target" / profile / "callix"
        if candidate.exists():
            return candidate
    return None


@pytest.mark.skipif(_binary() is None, reason="build the CLI first: task cli")
def test_lsif_has_ranges_on_import_and_export_lines(tmp_path: Path) -> None:
    write(tmp_path, B_TS)
    out = tmp_path / "out.lsif"
    subprocess.run(
        [str(_binary()), "export", str(tmp_path), "-l", "typescript", "-f", "lsif", "-q", "-o", str(out)],
        check=True,
        capture_output=True,
    )
    vertices = [json.loads(line) for line in out.read_text().splitlines() if line.strip()]
    docs = {v["id"]: v["uri"] for v in vertices if v.get("label") == "document"}
    b_id = next(i for i, uri in docs.items() if uri.endswith("b.ts"))
    contained = set()
    for v in vertices:
        if v.get("label") == "contains" and v.get("outV") == b_id:
            contained.update(v["inVs"])
    lines = {
        v["start"]["line"]
        for v in vertices
        if v.get("label") == "range"
        and v["id"] in contained
        and (v["start"]["line"], v["start"]["character"]) != (0, 0)
    }
    assert {0, 1, 3} <= lines
