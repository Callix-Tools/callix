"""What the author wrote about a definition is kept on its node.

A graph with the names and positions of a codebase and none of its comments is
half a map: `metadata["doc"]` carries the docstring of a Python function or class
and the comment block directly above a definition in every language, markers
removed. A comment that trails some other statement, or that is separated from
the definition by a blank line, is not documentation of it.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind


def docs(graph: callix.Graph) -> dict[str, str | None]:
    return {
        n.qualified_name: n.metadata.get("doc")
        for n in graph.nodes.values()
        if n.kind
        in {NodeKind.FUNCTION, NodeKind.METHOD, NodeKind.CLASS, NodeKind.VARIABLE, NodeKind.ATTRIBUTE}
    }


def test_python_docstrings_and_comments(tmp_path: Path) -> None:
    (tmp_path / "a.py").write_text(
        "# Helper used by the tests.\n"
        "def helper(): ...\n\n\n"
        "def create(name):\n"
        '    """Create a member.\n\n    Raises ValueError if the name is empty.\n    """\n\n\n'
        "# Two-line comment\n# above a class\n"
        "class Service:\n"
        '    """The service."""\n\n'
        "    def run(self):  # trailing, not documentation\n        pass\n\n"
        "    # Runs it twice.\n    @staticmethod\n    def twice(): ...\n\n\n"
        "x = 1  # about x\n\n"
        "# Documents y\ny = 2\n"
    )
    found = docs(callix.PythonAdapter(resolve=False).analyze(str(tmp_path)))
    assert found["a.helper"] == "Helper used by the tests."
    assert found["a.create"] == "Create a member.\n\nRaises ValueError if the name is empty."
    # A docstring is the documentation; the comment above the class is not.
    assert found["a.Service"] == "The service."
    assert found["a.Service.run"] is None
    # A comment above a decorator is above the decorated method.
    assert found["a.Service.twice"] == "Runs it twice."
    assert found["a.x"] is None
    assert found["a.y"] == "Documents y"


def test_a_blank_line_ends_the_block(tmp_path: Path) -> None:
    (tmp_path / "a.py").write_text("# Not about f: a blank line follows.\n\ndef f(): ...\n")
    assert docs(callix.PythonAdapter(resolve=False).analyze(str(tmp_path)))["a.f"] is None


def test_jsdoc_and_line_comments_in_typescript(tmp_path: Path) -> None:
    (tmp_path / "a.ts").write_text(
        "/**\n * Adds two numbers.\n * @param a first\n */\n"
        "export function add(a: number, b: number) { return a + b; }\n\n"
        "// Plain line comment\nexport class Box {\n  /** The size. */\n  size = 1;\n}\n"
    )
    found = docs(callix.TypeScriptAdapter(resolve=False).analyze(str(tmp_path)))
    assert found["a.add"] == "Adds two numbers.\n@param a first"
    assert found["a.Box"] == "Plain line comment"
    assert found["a.Box.size"] == "The size."


def test_go_doc_comments(tmp_path: Path) -> None:
    (tmp_path / "go.mod").write_text("module example.com/m\n\ngo 1.21\n")
    (tmp_path / "a.go").write_text(
        "package m\n\n// Add returns the sum.\n// It never overflows.\n"
        "func Add(a, b int) int { return a + b }\n\n// Q is a thing.\ntype Q struct{}\n"
    )
    found = docs(callix.GoAdapter(resolve=False).analyze(str(tmp_path)))
    assert found["example.com/m.Add"] == "Add returns the sum.\nIt never overflows."
    assert found["example.com/m.Q"] == "Q is a thing."


def test_rust_doc_comments_across_attributes(tmp_path: Path) -> None:
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "m"\nversion = "0.1.0"\nedition = "2021"\n'
    )
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "lib.rs").write_text(
        "/// Adds one.\n///\n/// Used everywhere.\n#[inline]\npub fn add_one(x: i32) -> i32 { x + 1 }\n\n"
        "/// A point.\n#[derive(Debug)]\npub struct Point;\n"
    )
    found = docs(callix.RustAdapter(resolve=False).analyze(str(tmp_path)))
    assert found["m::add_one"] == "Adds one.\n\nUsed everywhere."
    assert found["m::Point"] == "A point."


def test_phpdoc(tmp_path: Path) -> None:
    (tmp_path / "a.php").write_text(
        "<?php\n/**\n * A user.\n */\nclass User {\n    /** Returns the name. */\n"
        '    public function name() { return "x"; }\n}\n'
    )
    found = docs(callix.PhpAdapter(resolve=False).analyze(str(tmp_path)))
    assert found["User"] == "A user."
    assert found["User\\name"] == "Returns the name."


def test_c_takes_the_comment_from_the_header_when_the_definition_has_none(
    tmp_path: Path,
) -> None:
    (tmp_path / "engine.h").write_text(
        "#pragma once\n\n/**\n * Builds an engine.\n * Returns NULL on failure.\n */\n"
        "int engine_build(int size);\n"
    )
    (tmp_path / "engine.c").write_text(
        '#include "engine.h"\n\nint engine_build(int size) { return size; }\n\n'
        "/* The definition's own comment wins. */\nint engine_free(int x) { return x; }\n"
    )
    found = docs(callix.CAdapter(resolve=False).analyze(str(tmp_path)))
    assert found["engine_build"] == "Builds an engine.\nReturns NULL on failure."
    assert found["engine_free"] == "The definition's own comment wins."
