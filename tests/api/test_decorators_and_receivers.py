"""What a definition depends on, beyond the body it is written with.

A route handler is wired up outside its body: by the decorator that registers
it, by the defaults of its parameters (`db=Depends(get_db)`), and by the chain
of calls it is built from. None of those was walked — a bare decorator recorded
nothing, a default was never visited, and a call's receiver was dropped, so
`session.query(Model).filter(cond()).all()` produced no edge at all — and a
FastAPI app came out as a set of functions with nothing between them.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind

APP = '''\
from fastapi import APIRouter, Depends, FastAPI

router = APIRouter(prefix="/users")
app = FastAPI()


def get_db():
    return object()


def audit(fn):
    return fn


class Meta(type):
    pass


@router.get("/{user_id}")
@audit
async def read_user(user_id: int, db=Depends(get_db)):
    return user_id


class Model(metaclass=Meta):
    pass


app.include_router(router, prefix="/api")
'''

CHAIN = '''\
class Model: ...


class Session:
    def query(self, m): return self
    def filter(self, c): return self
    def all(self): return []


def cond(): ...


session = Session()


def use():
    return session.query(Model).filter(cond()).all()


def fresh():
    return Session().query(Model)
'''


def analyse(tmp_path: Path, source: str) -> callix.Graph:
    (tmp_path / "app.py").write_text(source)
    return callix.PythonAdapter().analyze(str(tmp_path))


def targets(
    graph: callix.Graph, source: str, kind: RelationKind, node_kind: NodeKind | None = None
) -> set[str]:
    # An import and the module it names can share a qualified name.
    (node,) = [
        n
        for n in graph.nodes.values()
        if n.qualified_name == source and (node_kind is None or n.kind is node_kind)
    ]
    return {
        graph.nodes[r.target_id].qualified_name
        for r in graph.relations
        if r.source_id == node.id and r.kind is kind
    }


def test_a_handler_depends_on_its_decorators_and_its_defaults(tmp_path: Path) -> None:
    graph = analyse(tmp_path, APP)
    depends_on = targets(graph, "app.read_user", RelationKind.REFERENCES)
    # `@router.get(...)` reads `router`, a bare `@audit` reads `audit`, and
    # `Depends(get_db)` in a default reads `get_db`.
    assert {"app.router", "app.audit", "app.get_db"} <= depends_on


def test_a_decorators_name_is_what_is_called(tmp_path: Path) -> None:
    graph = analyse(tmp_path, APP)
    (handler,) = [n for n in graph.nodes.values() if n.qualified_name == "app.read_user"]
    assert handler.metadata["decorators"] == ["router.get", "audit"]


def test_a_class_keyword_argument_is_used(tmp_path: Path) -> None:
    graph = analyse(tmp_path, APP)
    assert "app.Meta" in targets(graph, "app.Model", RelationKind.REFERENCES)


def test_a_call_chain_keeps_its_receivers_and_arguments(tmp_path: Path) -> None:
    graph = analyse(tmp_path, CHAIN)
    reads = targets(graph, "app.use", RelationKind.REFERENCES)
    calls = targets(graph, "app.use", RelationKind.CALLS)
    assert {"app.session", "app.Model"} <= reads
    assert {"app.Session.query", "app.cond"} <= calls
    # A call on a freshly built object still names the object's class.
    assert "app.Session" in targets(graph, "app.fresh", RelationKind.CALLS)


def test_a_route_belongs_to_its_handler_under_the_routers_prefix(tmp_path: Path) -> None:
    graph = analyse(tmp_path, APP)
    exposed = [
        (graph.nodes[r.source_id], graph.nodes[r.target_id])
        for r in graph.relations
        if r.kind is RelationKind.EXPOSES
    ]
    # The handler, not the file; the prefix on the router and the one it was
    # mounted with are both in the path.
    assert [(s.kind, s.qualified_name, b.name) for s, b in exposed] == [
        (NodeKind.FUNCTION, "app.read_user", "GET /api/users/{}"),
    ]


NEST = '''\
function Controller(p: string) { return (c: any) => c; }
function Get(p: string) { return (t: any, k: string) => {}; }
function Guard(t: any, k: string) {}
function Inject(n: string) { return (t: any, k: string, i: number) => {}; }
class Repo { find(id: number) { return id; } }
function dep() { return 1; }
function build() { return new Repo(); }

@Controller("users")
export class UsersController {
  constructor(private repo: Repo) {}

  @Get(":id")
  @Guard
  one(@Inject("x") id: number, opt = dep()) {
    return this.repo.find(id);
  }
}
export const x = build().find(1);
'''


def analyse_ts(tmp_path: Path) -> callix.Graph:
    (tmp_path / "a.ts").write_text(NEST)
    return callix.TypeScriptAdapter().analyze(str(tmp_path))


def test_typescript_decorators_defaults_and_receivers(tmp_path: Path) -> None:
    graph = analyse_ts(tmp_path)
    (method,) = [n for n in graph.nodes.values() if n.qualified_name == "a.UsersController.one"]
    (cls,) = [n for n in graph.nodes.values() if n.qualified_name == "a.UsersController"]
    assert method.metadata["decorators"] == ["Get", "Guard"]
    assert cls.metadata["decorators"] == ["Controller"]

    method_edges = targets(graph, "a.UsersController.one", RelationKind.CALLS) | targets(
        graph, "a.UsersController.one", RelationKind.REFERENCES
    )
    # The method decorators (`@Get`, `@Guard`), the parameter's (`@Inject`) and
    # the parameter's default (`dep()`).
    assert {"a.Get", "a.Guard", "a.Inject", "a.dep"} <= method_edges
    assert "a.Controller" in targets(graph, "a.UsersController", RelationKind.CALLS)
    # The receiver of `build().find(1)` is a call too.
    assert "a.build" in targets(graph, "a.ts", RelationKind.CALLS)


def test_a_nest_route_belongs_to_its_method_under_the_controllers_prefix(
    tmp_path: Path,
) -> None:
    graph = analyse_ts(tmp_path)
    exposed = [
        (graph.nodes[r.source_id], graph.nodes[r.target_id])
        for r in graph.relations
        if r.kind is RelationKind.EXPOSES
    ]
    assert [(s.qualified_name, b.name) for s, b in exposed] == [
        ("a.UsersController.one", "GET /users/{}"),
    ]


def test_a_rust_route_attribute_belongs_to_its_function(tmp_path: Path) -> None:
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "m"\nversion = "0.1.0"\nedition = "2021"\n'
    )
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "lib.rs").write_text(
        'pub fn guard() {}\n\n#[get("/users/{id}")]\npub async fn show() {}\n'
    )
    graph = callix.RustAdapter(resolve=False).analyze(str(tmp_path))
    exposed = [
        (graph.nodes[r.source_id], graph.nodes[r.target_id])
        for r in graph.relations
        if r.kind is RelationKind.EXPOSES
    ]
    # The attribute is written before the function, outside its span; the
    # route still belongs to the function, not to the file.
    assert [(s.kind, s.qualified_name, b.name) for s, b in exposed] == [
        (NodeKind.FUNCTION, "m::show", "GET /users/{}"),
    ]


def test_an_import_is_bound_whatever_order_the_files_are_read_in(tmp_path: Path) -> None:
    """`app/` sorts before `service/`, so the module is not in the graph yet.

    `from service.group_member import create` is bound to the module the name
    comes from, not to an external symbol; and the package's own
    `from service.group_member.create import create` is bound to the submodule
    it names, not to the package it sits in. Both then resolve to the function.
    """
    (tmp_path / "src" / "app").mkdir(parents=True)
    (tmp_path / "src" / "service" / "group_member").mkdir(parents=True)
    (tmp_path / "src" / "service" / "group_member" / "__init__.py").write_text(
        "from service.group_member.create import create\n"
    )
    (tmp_path / "src" / "service" / "group_member" / "create.py").write_text(
        "def create(name: str) -> dict:\n    return {'name': name}\n"
    )
    (tmp_path / "src" / "app" / "group_member.py").write_text(
        "from service.group_member import create\n\n\ndef handler(name: str):\n    return create(name)\n"
    )
    graph = callix.PythonAdapter().analyze(str(tmp_path))

    modules = {n.qualified_name for n in graph.nodes.values() if n.kind is NodeKind.MODULE}
    assert "service.group_member.create" in modules
    # Nothing internal is left as an external symbol.
    leftover = [
        n.qualified_name
        for n in graph.nodes.values()
        if n.kind is NodeKind.EXTERNAL_SYMBOL and n.metadata.get("origin") == "internal"
    ]
    assert leftover == []

    for importer in ("app.group_member.create", "service.group_member.create"):
        resolved = targets(graph, importer, RelationKind.RESOLVES_TO, NodeKind.IMPORT)
        # The module the import names, and the function it brings in.
        assert resolved == {"service.group_member.create", "service.group_member.create.create"}, importer
    assert targets(graph, "app.group_member.handler", RelationKind.CALLS) == {
        "service.group_member.create.create"
    }


def test_a_routers_prefix_comes_from_the_module_that_mounts_it(tmp_path: Path) -> None:
    """Routers built in one module and mounted in another, two levels deep."""
    package = tmp_path / "app" / "routers"
    package.mkdir(parents=True)
    (tmp_path / "app" / "__init__.py").write_text("")
    (package / "__init__.py").write_text("")
    (tmp_path / "app" / "main.py").write_text(
        "from fastapi import FastAPI\n"
        "from .routers import users, v1\n"
        "from .routers.items import router as items_router\n\n"
        "app = FastAPI()\n"
        'app.include_router(users.router, prefix="/api")\n'
        "app.include_router(items_router)\n"
        'app.include_router(v1.v1, prefix="/api")\n'
    )
    (package / "users.py").write_text(
        "from fastapi import APIRouter\n"
        'router = APIRouter(prefix="/users")\n\n'
        '@router.get("/{user_id}")\n'
        "def one(user_id: int): ...\n"
    )
    (package / "items.py").write_text(
        "from fastapi import APIRouter\n"
        'router = APIRouter(prefix="/items")\n\n'
        '@router.get("/")\n'
        "def lst(): ...\n"
    )
    (package / "v1.py").write_text(
        "from fastapi import APIRouter\n"
        "from . import users\n"
        'v1 = APIRouter(prefix="/v1")\n'
        "v1.include_router(users.router)\n\n"
        '@v1.get("/health")\n'
        "def health(): ...\n"
    )
    graph = callix.PythonAdapter().analyze(str(tmp_path))
    exposed = {
        graph.nodes[r.source_id].qualified_name: graph.nodes[r.target_id].name
        for r in graph.relations
        if r.kind is RelationKind.EXPOSES
    }
    assert exposed == {
        "app.routers.users.one": "GET /api/users/{}",
        "app.routers.items.lst": "GET /items",
        # `v1` is mounted under /api, so its own routes are /api/v1/...
        "app.routers.v1.health": "GET /api/v1/health",
    }
