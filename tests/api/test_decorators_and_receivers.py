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


def targets(graph: callix.Graph, source: str, kind: RelationKind) -> set[str]:
    (node,) = [n for n in graph.nodes.values() if n.qualified_name == source]
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
