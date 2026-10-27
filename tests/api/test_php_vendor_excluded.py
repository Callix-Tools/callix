"""Installed Composer packages under vendor/ are never PHP sub-roots."""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind


def write(root: Path, rel: str, text: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def layout(root: Path) -> None:
    write(root, "composer.json", '{"name":"app/app"}')
    write(root, "src/a.php", "<?php\nfunction run() { return 1; }\n")
    write(root, "vendor/acme/lib/composer.json", '{"name":"acme/lib"}')
    write(
        root,
        "vendor/acme/lib/src/Dep.php",
        "<?php\nnamespace Acme;\nclass Dep { public function go() { return 1; } }\n",
    )


def test_vendor_packages_are_not_roots(tmp_path: Path) -> None:
    layout(tmp_path)
    graph = callix.PhpAdapter().analyze(str(tmp_path))
    projects = [n.qualified_name for n in graph.nodes.values() if n.kind is NodeKind.PROJECT]
    assert projects == ["app/app"]
    assert not [n for n in graph.nodes.values() if (n.file_path or "").startswith("vendor/")]
    assert not [
        n for n in graph.nodes.values() if n.kind is NodeKind.CLASS and n.qualified_name == "Acme\\Dep"
    ]


def test_nested_composer_project_is_still_a_root_and_its_vendor_is_not(tmp_path: Path) -> None:
    layout(tmp_path)
    write(tmp_path, "packages/x/composer.json", '{"name":"app/x"}')
    write(tmp_path, "packages/x/src/X.php", "<?php\nclass X {}\n")
    write(tmp_path, "packages/x/vendor/foo/bar/composer.json", '{"name":"foo/bar"}')
    write(tmp_path, "packages/x/vendor/foo/bar/src/Bar.php", "<?php\nclass Bar {}\n")
    graph = callix.PhpAdapter().analyze(str(tmp_path))
    projects = {n.qualified_name for n in graph.nodes.values() if n.kind is NodeKind.PROJECT}
    assert projects == {"app/app", "app/x"}
    paths = {n.file_path for n in graph.nodes.values() if n.kind is NodeKind.FILE}
    assert "packages/x/src/X.php" in paths
    assert not [p for p in paths if p and (p.startswith("vendor/") or "/vendor/" in p)]
    roots = {Path(p) for p in callix.find_php_roots(tmp_path)}
    assert roots == {tmp_path, tmp_path / "packages/x"}


def test_a_use_of_a_vendor_class_is_external(tmp_path: Path) -> None:
    layout(tmp_path)
    write(
        tmp_path,
        "src/a.php",
        "<?php\nnamespace App;\nuse Acme\\Dep;\nfunction run() { $d = new Dep(); return $d->go(); }\n",
    )
    graph = callix.PhpAdapter().analyze(str(tmp_path))
    found = [
        r
        for r in graph.relations
        if r.kind is RelationKind.CALLS and graph.nodes[r.source_id].qualified_name == "App\\run"
    ]
    assert found
    for r in found:
        assert graph.nodes[r.target_id].kind is NodeKind.EXTERNAL_SYMBOL
        assert r.metadata["resolution"] == "external"
        assert "span" in r.metadata


def test_analysing_a_vendor_package_directly_still_works(tmp_path: Path) -> None:
    layout(tmp_path)
    graph = callix.PhpAdapter().analyze(str(tmp_path / "vendor/acme/lib"))
    assert {n.qualified_name for n in graph.nodes.values() if n.kind is NodeKind.PROJECT} == {"acme/lib"}
    assert any(n.kind is NodeKind.CLASS and n.qualified_name == "Acme\\Dep" for n in graph.nodes.values())
