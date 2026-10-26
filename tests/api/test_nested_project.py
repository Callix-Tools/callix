"""A project inside the directory analysed, and what its authors wrote.

`repo/backend/src` is where `service` and `api` are imported from, and `repo` is
the directory handed to callix: the type checker has to be told where first-party
modules are, or every import in the project misses.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

FILES = {
    "backend/pyproject.toml": '[project]\nname = "backend"\nversion = "0.1.0"\n',
    "backend/src/service/__init__.py": "",
    "backend/src/service/group_member/__init__.py": (
        "from service.group_member.create import create\n"
        "\n"
        '__all__ = ["create"]\n'
    ),
    "backend/src/service/group_member/create.py": (
        '"""Creating a member."""\n'
        "\n"
        "\n"
        "# Why this exists: the API cannot talk to the database itself.\n"
        "async def create(session, member):\n"
        '    """Adds the member and returns it."""\n'
        "    # a second explanation, inside the body\n"
        "    session.add(member)  # the commit happens in the caller\n"
        "    return member\n"
    ),
    "backend/src/api/__init__.py": "",
    "backend/src/api/group_member.py": (
        "from service.group_member import create\n"
        "\n"
        "\n"
        "async def create_group_member(session, member):\n"
        "    return await create(session, member)\n"
    ),
}


def analyse(tmp_path: Path) -> callix.Graph:
    for name, text in FILES.items():
        path = tmp_path / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return callix.PythonAdapter().analyze(str(tmp_path))


def edges(graph: callix.Graph, kind: RelationKind) -> set[tuple[str, str]]:
    return {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is kind
    }


def test_a_call_through_a_package_that_reexports_a_function_of_the_same_name(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    assert ("api.group_member.create_group_member", "service.group_member.create.create") in edges(
        graph, RelationKind.CALLS
    )


def test_the_import_is_the_function_the_package_binds_not_the_submodule(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    resolved = {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].kind, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.RESOLVES_TO
    }
    assert ("api.group_member.create", NodeKind.FUNCTION, "service.group_member.create.create") in resolved
    # `create` is also a submodule, but `from service.group_member import create`
    # reads the package's own `create`.
    assert ("api.group_member.create", NodeKind.MODULE, "service.group_member.create") not in resolved


def test_the_docstring_and_the_comment_above_are_both_kept(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    create = next(n for n in graph.nodes.values() if n.qualified_name == "service.group_member.create.create")
    assert create.metadata["doc"] == "Adds the member and returns it."
    assert create.metadata["leading_comment"] == "Why this exists: the API cannot talk to the database itself."


def test_the_comments_in_the_body_are_kept_with_their_lines(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    create = next(n for n in graph.nodes.values() if n.qualified_name == "service.group_member.create.create")
    texts = {c["line"]: c["text"] for c in create.metadata["comments"]}
    assert texts[7] == "a second explanation, inside the body"
    assert texts[8] == "the commit happens in the caller"


def test_a_module_docstring_is_the_documentation_of_the_module(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    module = next(n for n in graph.nodes.values() if n.kind is NodeKind.MODULE and n.qualified_name == "service.group_member.create")
    assert module.metadata["doc"] == "Creating a member."
