"""The native engine against the Python one.

The `callix` command line is the same adapters built without PyO3, with its own
JSON writer. This runs both over every fixture and requires the same bytes —
the property that lets a graph from the command line stand in for one from the
library. Only wall-clock timings are allowed to differ.

Skipped when the binary has not been built (`task cli`).
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
from pathlib import Path

import pytest

import callix
from tests.graphs import FIXTURES

REPO = Path(__file__).resolve().parents[2]


def _binary() -> Path | None:
    override = os.environ.get("CALLIX_CLI")
    if override:
        return Path(override)
    for profile in ("release", "debug"):
        candidate = REPO / "target" / profile / "callix"
        if candidate.exists():
            return candidate
    return None


CLI = _binary()
pytestmark = pytest.mark.skipif(CLI is None, reason="build the CLI first: task cli")

ADAPTERS = {
    "python": callix.PythonAdapter,
    "typescript": callix.TypeScriptAdapter,
    "go": callix.GoAdapter,
    "rust": callix.RustAdapter,
    "php": callix.PhpAdapter,
    "c": callix.CAdapter,
    "cpp": callix.CppAdapter,
    "yaml": callix.YamlAdapter,
}

#: Resolved with a resolver that is linked in, so it needs no toolchain.
LINKED_IN = ["python", "typescript", "php", "c", "cpp", "yaml"]


def _toolchain(program: str, *args: str) -> bool:
    if shutil.which(program) is None:
        return False
    try:
        return subprocess.run(
            [program, *args], capture_output=True, timeout=30, check=False
        ).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def _timings_removed(text: str) -> str:
    return re.sub(r'"seconds": [0-9.eE+-]+', '"seconds": 0', text)


def _cli_graph(language: str, *flags: str) -> str:
    out = subprocess.run(
        [str(CLI), "analyze", str(FIXTURES / language), "-q", "-l", language, *flags],
        capture_output=True,
        text=True,
        check=True,
    )
    return out.stdout.rstrip("\n")


@pytest.mark.parametrize("language", sorted(ADAPTERS))
def test_structural_graphs_are_byte_identical(language: str) -> None:
    expected = ADAPTERS[language](resolve=False).analyze(str(FIXTURES / language)).to_json()
    assert _cli_graph(language, "--no-resolve") == expected


@pytest.mark.parametrize("language", LINKED_IN)
def test_resolved_graphs_are_byte_identical(language: str) -> None:
    expected = ADAPTERS[language]().analyze(str(FIXTURES / language)).to_json()
    assert _timings_removed(_cli_graph(language)) == _timings_removed(expected)


def test_go_resolved_graph_is_byte_identical() -> None:
    if not _toolchain("go", "version"):
        pytest.skip("no Go toolchain")
    expected = ADAPTERS["go"]().analyze(str(FIXTURES / "go")).to_json()
    assert _timings_removed(_cli_graph("go")) == _timings_removed(expected)


def test_rust_resolved_graph_is_byte_identical() -> None:
    if not _toolchain("rust-analyzer", "--version"):
        pytest.skip("no usable rust-analyzer")
    expected = ADAPTERS["rust"]().analyze(str(FIXTURES / "rust")).to_json()
    assert _timings_removed(_cli_graph("rust")) == _timings_removed(expected)


def test_indented_output_matches_json_dumps() -> None:
    expected = ADAPTERS["python"](resolve=False).analyze(str(FIXTURES / "python")).to_json(indent=2)
    assert _cli_graph("python", "--no-resolve", "--pretty") == expected


def test_a_graph_from_the_cli_reads_back_in_python() -> None:
    graph = callix.Graph.from_json(_cli_graph("python"))
    assert graph.metadata["resolver_status"] == "ok"
    assert len(graph.nodes) > 30
