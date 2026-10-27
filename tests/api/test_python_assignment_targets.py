"""Where an assignment target is declared.

`self.x` / `cls.x` is one ATTRIBUTE of the enclosing class, `obj.x = v` declares
nothing (it writes the existing attribute), every target of `a = b = v` is
declared, and a module-level `for` / `except ... as` target is a module variable.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

MODELS = '''\
class Query:
    end_time = None


class Base:
    def reset(self):
        self.owner = None

    def touch(self):
        self.owner = 1
'''

RUN = '''\
from pkg.models import Query


def run(query: Query):
    query.end_time = 1.0
    a = b = 2
    return a, b
'''

MOD = '''\
items = [1]
for key in items:
    pass
try:
    pass
except ValueError as e:
    pass
'''

CLS = '''\
class C:
    k = 1

    @classmethod
    def make(cls):
        cls.w = 1

    def m(self):
        def inner():
            self.deep = 2
        return inner


def f(x):
    x.y.z = 1
'''


def analyse(tmp_path: Path, *, resolve: bool = False) -> callix.Graph:
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "__init__.py").write_text("")
    (pkg / "models.py").write_text(MODELS)
    (pkg / "run.py").write_text(RUN)
    (pkg / "cls.py").write_text(CLS)
    (tmp_path / "mod.py").write_text(MOD)
    return callix.PythonAdapter(resolve=resolve).analyze(str(tmp_path))


def names(graph: callix.Graph, *kinds: NodeKind) -> set[str]:
    return {n.qualified_name for n in graph.nodes.values() if n.kind in kinds}


def test_an_attribute_write_through_another_receiver_declares_nothing(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    assert not [n for n in graph.nodes.values() if n.qualified_name.startswith("pkg.run.run.") and n.name == "end_time"]
    assert "pkg.run.run.end_time" not in names(graph, NodeKind.VARIABLE, NodeKind.ATTRIBUTE)


def test_self_attribute_is_one_node_declared_by_the_class(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    owners = [n for n in graph.nodes.values() if n.qualified_name.startswith("pkg.models.Base.") and n.name == "owner"]
    assert [(n.qualified_name, n.kind) for n in owners] == [("pkg.models.Base.owner", NodeKind.ATTRIBUTE)]
    declares = [
        r
        for r in graph.relations
        if r.kind is RelationKind.DECLARES and r.target_id == owners[0].id
    ]
    assert len(declares) == 1
    assert graph.nodes[declares[0].source_id].qualified_name == "pkg.models.Base"
    # Both write sites are known to the resolver.
    assert len(owners[0].metadata["declared_at"]) >= 2


def test_every_target_of_a_chained_assignment_is_declared(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    variables = names(graph, NodeKind.VARIABLE)
    assert {"pkg.run.run.a", "pkg.run.run.b"} <= variables


def test_module_level_for_and_except_targets_are_variables(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    assert {"mod.key", "mod.e"} <= names(graph, NodeKind.VARIABLE)


def test_cls_and_closure_targets_belong_to_the_class(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    attributes = names(graph, NodeKind.ATTRIBUTE)
    assert {"pkg.cls.C.w", "pkg.cls.C.deep", "pkg.cls.C.k"} <= attributes
    assert not {a for a in attributes if ".make." in a or ".inner." in a or ".m." in a}


def test_a_dotted_receiver_declares_no_node_and_is_read(tmp_path: Path) -> None:
    graph = analyse(tmp_path)
    assert not [n for n in graph.nodes.values() if n.qualified_name.startswith("pkg.cls.f.") and n.name == "z"]


def test_the_write_through_a_receiver_references_the_attribute(tmp_path: Path) -> None:
    graph = analyse(tmp_path, resolve=True)
    writes = [
        r
        for r in graph.relations
        if r.kind is RelationKind.REFERENCES
        and r.metadata.get("access") == "write"
        and graph.nodes[r.target_id].qualified_name == "pkg.models.Query.end_time"
    ]
    assert writes, "query.end_time = ... must write Query.end_time"
    assert all("span" in r.metadata for r in writes)
