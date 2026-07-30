"""Do the docs name methods that exist?

Every ```python block in the docs, the README and CONTRIBUTING is scanned for
attribute access on a receiver whose type is known — `graph.`, `node.`, an
adapter — and each name is checked against the built module.

This is here because of a specific failure: `Graph.find_by_name` and
`Graph.file_subgraph` were documented for months under names the code never had
(`nodes_by_name`, `subgraph_for_file`). Nothing caught it, because a doc example
is not executed and prose is not type-checked. A reader who copied either line
got an `AttributeError`.

Only fenced Python is scanned, deliberately. Prose mentions filenames
(`src/graph.rs`) and hypothetical APIs, and treating those as claims produced
more false alarms than findings.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

import callix

REPO = Path(__file__).resolve().parents[2]

DOC_FILES = sorted((REPO / "website" / "docs").rglob("*.md")) + [
    REPO / "README.md",
    REPO / "CONTRIBUTING.md",
]

# A receiver name used in the examples -> the type it stands for.
RECEIVERS = {
    "graph": callix.Graph,
    "old": callix.Graph,
    "new": callix.Graph,
    "node": callix.Node,
    "boundary": callix.Node,
    "relation": callix.Relation,
    "edge": callix.Relation,
    "span": callix.Span,
    "diff": callix.GraphDiff,
    "metrics": callix.ResolverMetrics,
    "index": callix.SpanIndex,
    # Adapters share one surface; any of them answers for the rest.
    "adapter": callix.PythonAdapter,
}

# Names the examples call on objects the *reader* defines: the resolver, parser
# and extractor protocols, plus framework calls that appear in sample code.
PROTOCOL = {
    "prepare",
    "resolve_all",
    "status",
    "definition_at",
    "can_parse",
    "parse",
    "extract",
}

# Open containers: whatever is read out of them is data, not API.
CONTAINERS = {"metadata", "nodes", "relations"}

PYTHON_BLOCK = re.compile(r"```python\n(.*?)```", re.DOTALL)
ACCESS = re.compile(r"\b([a-z_]+)\.([a-z_][a-z_0-9]*)\b")


def python_blocks(text: str) -> list[str]:
    return PYTHON_BLOCK.findall(text)


def public_attrs(cls: type) -> set[str]:
    return {name for name in dir(cls) if not name.startswith("_")}


@pytest.mark.parametrize("path", DOC_FILES, ids=lambda p: p.name)
def test_documented_attributes_exist(path: Path):
    missing: list[str] = []
    for block in python_blocks(path.read_text()):
        for receiver, attribute in ACCESS.findall(block):
            cls = RECEIVERS.get(receiver)
            if cls is None or attribute in PROTOCOL or attribute in CONTAINERS:
                continue
            if attribute not in public_attrs(cls):
                missing.append(f"{receiver}.{attribute} (no such member on {cls.__name__})")

    assert not missing, f"{path.relative_to(REPO)} documents names that do not exist: " + ", ".join(
        sorted(set(missing))
    )


def test_the_scan_reaches_the_docs():
    """A guard on the guard: an empty scan would pass every test above."""
    blocks = sum(len(python_blocks(path.read_text())) for path in DOC_FILES)
    assert blocks > 40, f"only {blocks} Python blocks found — has the docs layout moved?"


def test_every_adapter_the_docs_name_is_exported():
    named = set()
    for path in DOC_FILES:
        named |= set(re.findall(r"\b([A-Z][A-Za-z]*Adapter)\b", path.read_text()))
    # `Adapter` alone is the docs' placeholder for "any of them".
    named -= {"Adapter"}
    assert named, "the docs name no adapters at all"
    unknown = sorted(name for name in named if not hasattr(callix, name))
    assert not unknown, f"documented but not exported: {unknown}"
