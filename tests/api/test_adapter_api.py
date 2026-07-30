"""The constructor surface every adapter shares, and what each argument does.

Seven adapters, one signature: `resolve`, `resolver`, `dep_parsers`,
`boundary_extractors`, all keyword-only. These tests are parametrized over every
one of them on purpose — `resolver=` and `dep_parsers=` were accepted by
`PythonAdapter` alone for most of the project's life, and the way that is caught
is by asking each adapter the same question rather than trusting that a shared
enum is wired up everywhere.

Nothing here needs a language toolchain: a custom resolver replaces the native
backend, so no adapter reaches for ty, typescript-go, `go/packages` or
rust-analyzer while these run.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import callix
from callix import AdapterError, NodeKind, ResolvedRef

from tests.graphs import FIXTURES

# (adapter class, fixture directory). YAML is deliberately absent — see
# test_yaml_takes_no_resolver.
ADAPTERS = [
    ("PythonAdapter", "python"),
    ("TypeScriptAdapter", "typescript"),
    ("GoAdapter", "go"),
    ("RustAdapter", "rust"),
    ("PhpAdapter", "php"),
    ("CAdapter", "c"),
    ("CppAdapter", "cpp"),
]


class Spy:
    """A resolver that answers every query with one synthetic definition.

    It also asserts the shape of what it is handed, because the protocol is part
    of the public API: `pathlib.Path` arguments, and 1-based coordinates.
    """

    def __init__(self, status: str = "ok") -> None:
        self.prepared: tuple[Path, int] | None = None
        self.queries = 0
        self._status = status

    def prepare(self, root, files) -> None:
        assert isinstance(root, Path), type(root)
        assert all(isinstance(f, Path) for f in files), files
        self.prepared = (root, len(files))

    def resolve_all(self, queries):
        self.queries += len(queries)
        for path, line, col in queries:
            assert isinstance(path, Path), type(path)
            assert line >= 1 and col >= 1, (line, col)
        return [
            ResolvedRef("spied.thing", None, 1, 1, "function", "third_party")
            for _ in queries
        ]

    def status(self) -> str:
        return self._status


class OneName:
    """A manifest parser that declares a single, unmistakable dependency."""

    def can_parse(self, project_root) -> bool:
        assert isinstance(project_root, Path), type(project_root)
        return True

    def parse(self, project_root):
        # A list, not a set: any iterable of names is accepted.
        return ["parsed-by-hand"]


def adapter_and_root(name: str, fixture: str):
    return getattr(callix, name), FIXTURES / fixture


@pytest.mark.parametrize(("name", "fixture"), ADAPTERS)
def test_a_custom_resolver_is_prepared_and_asked(name, fixture):
    cls, root = adapter_and_root(name, fixture)
    spy = Spy()
    graph = cls(resolver=spy).analyze(root)

    assert spy.prepared is not None, "prepare was never called"
    assert spy.queries > 0, "resolve_all was never called"
    assert graph.metadata["resolver_status"] == "ok"
    assert graph.metadata["resolver_metrics"]["queries"] == spy.queries


@pytest.mark.parametrize(("name", "fixture"), ADAPTERS)
def test_a_custom_resolvers_answer_becomes_an_edge(name, fixture):
    cls, root = adapter_and_root(name, fixture)
    graph = cls(resolver=Spy()).analyze(root)

    spied = [
        node
        for node in graph.nodes_by_kind(NodeKind.EXTERNAL_SYMBOL)
        if node.qualified_name == "spied.thing"
    ]
    assert len(spied) == 1, "the resolver's answer produced no node"
    assert graph.incoming(spied[0].id), "no edge points at the resolved node"


@pytest.mark.parametrize(("name", "fixture"), ADAPTERS)
def test_custom_manifest_parsers_replace_the_built_in_reader(name, fixture):
    cls, root = adapter_and_root(name, fixture)
    graph = cls(resolve=False, dep_parsers=[OneName()]).analyze(root)

    declared = sorted(
        node.qualified_name for node in graph.nodes_by_kind(NodeKind.DEPENDENCY)
    )
    assert declared == ["parsed-by-hand"]


@pytest.mark.parametrize(("name", "fixture"), ADAPTERS)
def test_resolve_false_wins_over_a_resolver(name, fixture):
    """The combination is a caller switching resolution off, not a request to
    resolve through the object they happened to leave in the constructor."""
    cls, root = adapter_and_root(name, fixture)
    spy = Spy()
    graph = cls(resolve=False, resolver=spy).analyze(root)

    assert spy.prepared is None and spy.queries == 0
    assert graph.metadata["resolver_status"] == "unavailable"


@pytest.mark.parametrize(("name", "fixture"), ADAPTERS)
def test_a_resolvers_status_reaches_strict_mode(name, fixture):
    cls, root = adapter_and_root(name, fixture)
    with pytest.raises(AdapterError, match="degraded"):
        cls(resolver=Spy(status="degraded")).analyze(root, strict=True)


@pytest.mark.parametrize(("name", "fixture"), ADAPTERS)
def test_a_short_answer_list_is_refused(name, fixture):
    """Answers are matched to queries by index, so a resolver that drops one
    would shift every later answer onto the wrong use-site."""
    cls, root = adapter_and_root(name, fixture)

    class Short(Spy):
        def resolve_all(self, queries):
            return [None] * (len(queries) - 1)

    with pytest.raises(AdapterError, match="answer every query"):
        cls(resolver=Short()).analyze(root)


@pytest.mark.parametrize(("name", "fixture"), ADAPTERS)
def test_the_repr_names_the_backend_in_use(name, fixture):
    cls, _ = adapter_and_root(name, fixture)
    assert repr(cls(resolver=Spy())).endswith("(resolver=custom)")
    assert repr(cls(resolve=False)).endswith("(resolver=off)")
    # The native backend names itself; which name is the adapter's business.
    assert "resolver=" in repr(cls())


@pytest.mark.parametrize(("name", "fixture"), ADAPTERS + [("YamlAdapter", "yaml")])
def test_every_argument_is_keyword_only(name, fixture):
    cls, _ = adapter_and_root(name, fixture)
    with pytest.raises(TypeError):
        cls(False)


def test_yaml_takes_no_resolver():
    """YAML declares no symbols, so there is no resolution phase to redirect.

    Accepting the argument and ignoring it would be worse than refusing it: a
    caller who passed a resolver would be told nothing while their resolver was
    never called.
    """
    with pytest.raises(TypeError, match="resolver"):
        callix.YamlAdapter(resolver=Spy())
    with pytest.raises(TypeError, match="dep_parsers"):
        callix.YamlAdapter(dep_parsers=[OneName()])
