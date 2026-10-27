"""A PHP `use` clause is placed and resolved to what it imports."""

from __future__ import annotations

import json
from pathlib import Path

import callix

COMPOSER = (
    '{"name":"app/app","require":{"monolog/monolog":"^3"},'
    '"autoload":{"psr-4":{"App\\\\":"src/"}}}'
)

BAR = """<?php
namespace App\\Sub;

use App\\Foo;
use App\\Foo as F2;
use App\\Contracts\\{Cache, Logger as Log};
use function App\\slugify;
use const App\\LIMIT;
use Monolog\\Logger;
use App\\Contracts;

class Bar {}
"""


def write(root: Path, rel: str, text: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def layout(root: Path) -> None:
    write(root, "composer.json", COMPOSER)
    write(
        root,
        "src/Foo.php",
        "<?php\nnamespace App;\n\nclass Foo {}\nfunction slugify() {}\nconst LIMIT = 1;\n",
    )
    write(
        root,
        "src/Contracts/Cache.php",
        "<?php\nnamespace App\\Contracts;\n\nclass Cache {}\nclass Logger {}\n",
    )
    write(root, "src/Bar.php", BAR)


def dump(root: Path, **kwargs: object) -> dict:
    graph = callix.PhpAdapter(**kwargs).analyze(str(root))
    return json.loads(graph.to_json())


def resolved_imports(data: dict) -> dict[str, list[dict]]:
    """IMPORT local qualified name -> its RESOLVES_TO edges that carry a span."""
    nodes = {n["id"]: n for n in data["nodes"]}
    out: dict[str, list[dict]] = {}
    for r in data["relations"]:
        src = nodes[r["source_id"]]
        if src["kind"] != "import" or r["kind"] != "resolves_to":
            continue
        if not (r.get("metadata") or {}).get("span"):
            continue
        out.setdefault(src["qualified_name"], []).append(
            {"edge": r, "target": nodes[r["target_id"]], "source": src}
        )
    return out


def find(found: dict[str, list[dict]], suffix: str) -> dict:
    keys = [k for k in found if k.endswith(suffix)]
    assert len(keys) == 1, (suffix, sorted(found))
    assert len(found[keys[0]]) == 1, (suffix, found[keys[0]])
    return found[keys[0]][0]


def test_use_clause_resolves_to_class_with_span(tmp_path: Path) -> None:
    layout(tmp_path)
    data = dump(tmp_path)
    found = resolved_imports(data)
    expected = {
        "Sub\\Foo": ("class", "App\\Foo", 4),
        "Sub\\F2": ("class", "App\\Foo", 5),
        "Sub\\Cache": ("class", "App\\Contracts\\Cache", 6),
        "Sub\\Log": ("class", "App\\Contracts\\Logger", 6),
        "Sub\\slugify": ("function", "App\\slugify", 7),
        "Sub\\LIMIT": None,
        "Sub\\Contracts": ("module", "App\\Contracts", 10),
    }
    for suffix, want in expected.items():
        item = find(found, suffix)
        edge, target, source = item["edge"], item["target"], item["source"]
        assert source["span"] is not None
        name_span = source["metadata"]["name_span"]
        assert edge["metadata"]["span"] == name_span
        assert edge["metadata"]["resolution"] == "definition"
        if want is None:
            assert target["qualified_name"].endswith("LIMIT")
            continue
        kind, qname, line = want
        assert target["kind"] == kind
        assert target["qualified_name"] == qname
        assert edge["metadata"]["span"]["__span__"][0] == line
    # The alias is not the position: `use App\Foo as F2;` is placed at `Foo`.
    alias = find(found, "Sub\\F2")
    plain = find(found, "Sub\\Foo")
    assert alias["edge"]["metadata"]["span"]["__span__"][1] == plain["edge"]["metadata"]["span"]["__span__"][1]
    # A vendor class is an external symbol, said so.
    vendor = find(found, "Sub\\Logger")
    assert vendor["target"]["kind"] == "external_symbol"
    assert vendor["target"]["qualified_name"] == "Monolog\\Logger"
    assert vendor["edge"]["metadata"]["resolution"] == "external"
    # The structural edge from the file to the namespace MODULE is kept.
    nodes = {n["id"]: n for n in data["nodes"]}
    assert any(
        r["kind"] == "imports"
        and nodes[r["source_id"]]["kind"] == "file"
        and nodes[r["target_id"]]["kind"] == "module"
        and nodes[r["target_id"]]["qualified_name"] == "App"
        for r in data["relations"]
    )


def test_use_clause_structure_without_resolution(tmp_path: Path) -> None:
    layout(tmp_path)
    data = dump(tmp_path, resolve=False)
    imports = [n for n in data["nodes"] if n["kind"] == "import"]
    # Eight clauses: the group `{Cache, Logger as Log}` contributes two.
    assert len(imports) == 8
    assert all(n["span"] is not None for n in imports)
    assert not resolved_imports(data)
    assert any(r["kind"] == "imports" for r in data["relations"])
