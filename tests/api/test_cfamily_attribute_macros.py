"""Project macros that annotate a declaration and expand to nothing.

`class MY_EXPORT Foo {}`, `int x GUARDED_BY(m)` and `API int (*f)(int)` are erased
before parsing, so the declaration is read as written.
"""

from __future__ import annotations

from pathlib import Path

import callix
from callix import NodeKind, RelationKind


def analyse(tmp_path: Path, files: dict[str, str], adapter: type) -> callix.Graph:
    for name, text in files.items():
        (tmp_path / name).write_text(text)
    return adapter().analyze(str(tmp_path))


def named(graph: callix.Graph, kind: NodeKind, qualified_name: str) -> list:
    return [n for n in graph.nodes.values() if n.kind is kind and n.qualified_name == qualified_name]


def calls_from(graph: callix.Graph, prefix: str) -> list:
    return [
        graph.nodes[r.target_id]
        for r in graph.relations
        if r.kind is RelationKind.CALLS and graph.nodes[r.source_id].qualified_name.startswith(prefix)
    ]


A_H = """#define MY_EXPORT
#define GUARDED_BY(x)
class MY_EXPORT Foo {
 public:
  void Bar();
  int x_ GUARDED_BY(m_);
  int m_;
};
"""
A_CC = '#include "a.h"\nvoid Foo::Bar() {}\nvoid use(Foo& f) { f.Bar(); }\n'


def test_exported_class_is_a_class(tmp_path: Path) -> None:
    graph = analyse(tmp_path, {"a.h": A_H, "a.cc": A_CC}, callix.CppAdapter)
    assert len(named(graph, NodeKind.CLASS, "Foo")) == 1
    assert not named(graph, NodeKind.FUNCTION, "Foo")
    # C++ qualified names carry the parameter list; declaration and definition are one node.
    assert len(named(graph, NodeKind.METHOD, "Foo::Bar()")) == 1
    assert named(graph, NodeKind.ATTRIBUTE, "Foo::x_")
    for node in graph.nodes.values():
        if node.kind is NodeKind.EXTERNAL_SYMBOL:
            assert not node.qualified_name.startswith(("MY_EXPORT", "GUARDED_BY"))
    targets = calls_from(graph, "use(")
    assert [t.qualified_name for t in targets] == ["Foo::Bar()"]
    assert all(t.kind is not NodeKind.EXTERNAL_SYMBOL for t in targets)


def test_c_api_macros_declare_what_follows(tmp_path: Path) -> None:
    files = {
        "api.h": "#define MOD_API\n#define LUA_API\nMOD_API int (*RM_OpenKey)(int mode);\nLUA_API int (lua_next)(int idx);\n",
        "use.c": '#include "api.h"\nint f(void) { return RM_OpenKey(1) + lua_next(2); }\n',
        "lapi.c": '#include "api.h"\nLUA_API int (lua_next)(int idx) { return idx; }\n',
    }
    graph = analyse(tmp_path, files, callix.CAdapter)
    keys = named(graph, NodeKind.VARIABLE, "RM_OpenKey")
    assert len(keys) == 1 and keys[0].file_path == "api.h"
    assert len(named(graph, NodeKind.FUNCTION, "lua_next")) == 1
    assert not named(graph, NodeKind.FUNCTION, "int")
    targets = calls_from(graph, "f")
    assert {t.qualified_name for t in targets} == {"lua_next", "RM_OpenKey"}
    assert all(t.kind is not NodeKind.EXTERNAL_SYMBOL for t in targets)


def test_storage_keyword_macro_keeps_extern(tmp_path: Path) -> None:
    files = {"api.h": "#define LUA_API extern\nLUA_API int counter;\n", "c.c": '#include "api.h"\nint counter = 0;\n'}
    graph = analyse(tmp_path, files, callix.CAdapter)
    counters = named(graph, NodeKind.VARIABLE, "counter")
    assert len(counters) == 1
    assert counters[0].file_path == "c.c"


def test_macro_in_preprocessor_lines_untouched(tmp_path: Path) -> None:
    source = "#define EXPORT\n#ifdef EXPORT\nclass EXPORT C { public: void m(); };\n#endif\n"
    graph = analyse(tmp_path, {"main.cpp": source}, callix.CppAdapter)
    assert named(graph, NodeKind.CLASS, "C")


def test_non_annotation_macro_not_erased(tmp_path: Path) -> None:
    source = "#define STATUS int\nSTATUS f(void) { return 1; }\nint main(void) { return f(); }\n"
    graph = analyse(tmp_path, {"main.c": source}, callix.CAdapter)
    assert [t.qualified_name for t in calls_from(graph, "main")] == ["f"]


def test_names_in_comments_and_strings_are_left_alone(tmp_path: Path) -> None:
    source = '#define EXPORT\n/* EXPORT doc */\nint f(void) { return 0; }\nconst char *s = "EXPORT";\nint n = 1\'000;\n'
    graph = analyse(tmp_path, {"main.c": source}, callix.CAdapter)
    assert named(graph, NodeKind.FUNCTION, "f")


EXPORT_H = """#ifndef EXPORT_H_
#define EXPORT_H_
#if !defined(LEVELDB_EXPORT)
#if defined(LEVELDB_SHARED_LIBRARY)
#if defined(_WIN32)
#if defined(LEVELDB_COMPILE_LIBRARY)
#define LEVELDB_EXPORT __declspec(dllexport)
#else
#define LEVELDB_EXPORT __declspec(dllimport)
#endif
#else
#define LEVELDB_EXPORT __attribute__((visibility("default")))
#endif
#else
#define LEVELDB_EXPORT
#endif
#endif
#endif
"""
THREAD_ANNOTATIONS_H = """#ifndef THREAD_ANNOTATIONS_H_
#define THREAD_ANNOTATIONS_H_
#if !defined(THREAD_ANNOTATION_ATTRIBUTE__)
#if defined(__clang__)
#define THREAD_ANNOTATION_ATTRIBUTE__(x) __attribute__((x))
#else
#define THREAD_ANNOTATION_ATTRIBUTE__(x)  // no-op
#endif
#endif
#ifndef GUARDED_BY
#define GUARDED_BY(x) THREAD_ANNOTATION_ATTRIBUTE__(guarded_by(x))
#endif
#endif
"""
DB_H = """#include "export.h"
#include "thread_annotations.h"
namespace leveldb {
class LEVELDB_EXPORT DB {
 public:
  virtual void Get();
  int n_ GUARDED_BY(mu_);
  int mu_;
};
}
"""
USE_CC = '#include "db.h"\nnamespace leveldb {\nvoid u(DB* d) { d->Get(); }\n}\n'


def test_macro_from_macro_only_header(tmp_path: Path) -> None:
    files = {"export.h": EXPORT_H, "thread_annotations.h": THREAD_ANNOTATIONS_H, "db.h": DB_H, "use.cc": USE_CC}
    graph = analyse(tmp_path, files, callix.CppAdapter)
    assert len(named(graph, NodeKind.CLASS, "leveldb::DB")) == 1
    assert not named(graph, NodeKind.FUNCTION, "leveldb::DB")
    assert [n for n in graph.nodes.values() if n.kind is NodeKind.METHOD and n.qualified_name.endswith("DB::Get()")]
    assert named(graph, NodeKind.ATTRIBUTE, "leveldb::DB::n_")
    for node in graph.nodes.values():
        if node.kind is NodeKind.EXTERNAL_SYMBOL:
            assert not node.qualified_name.startswith(("LEVELDB_EXPORT", "GUARDED_BY"))
    targets = calls_from(graph, "leveldb::u(")
    assert targets and all(t.kind is not NodeKind.EXTERNAL_SYMBOL for t in targets)
    assert any(t.qualified_name.endswith("DB::Get()") for t in targets)
    # No C adapter reads this tree (no `.c` file), so the macro-only header is C++'s.
    cpp_files = {n.file_path for n in graph.nodes.values() if n.kind is NodeKind.FILE}
    assert "export.h" in cpp_files
    # Once a C source exists, the macro-only header is C's again and C++ reads it
    # for erasure only (the foreign-macro path).
    (tmp_path / "c_side.c").write_text("int c_side(void) { return 0; }\n")
    c_graph = callix.CAdapter().analyze(str(tmp_path))
    c_files = {n.file_path for n in c_graph.nodes.values() if n.kind is NodeKind.FILE}
    assert "export.h" in c_files
    assert "db.h" not in c_files
    graph2 = callix.CppAdapter().analyze(str(tmp_path))
    cpp_files2 = {n.file_path for n in graph2.nodes.values() if n.kind is NodeKind.FILE}
    assert "export.h" not in cpp_files2
    assert len(named(graph2, NodeKind.CLASS, "leveldb::DB")) == 1
    assert not named(graph2, NodeKind.FUNCTION, "leveldb::DB")


def test_c_macro_from_header_cpp_claims(tmp_path: Path) -> None:
    files = {
        "mod.h": "#define MOD_API\n#ifdef __cplusplus\ntemplate <typename T> class Q { public: T t; };\n#endif\n",
        "api.h": '#include "mod.h"\nMOD_API int (*RM_OpenKey)(int mode);\n',
        "use.c": '#include "api.h"\nint f(void) { return RM_OpenKey(1); }\n',
    }
    graph = analyse(tmp_path, files, callix.CAdapter)
    c_files = {n.file_path for n in graph.nodes.values() if n.kind is NodeKind.FILE}
    assert "mod.h" not in c_files  # the probe really gave it to C++
    assert len(named(graph, NodeKind.VARIABLE, "RM_OpenKey")) == 1
    targets = calls_from(graph, "f")
    assert [t.qualified_name for t in targets] == ["RM_OpenKey"]
    assert all(t.kind is not NodeKind.EXTERNAL_SYMBOL for t in targets)
