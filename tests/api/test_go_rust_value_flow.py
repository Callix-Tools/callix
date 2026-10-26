"""Which function a Go or Rust call reaches when the callee is a value.

Containers looped over, channels, variadic parameters, the standard library's
own callbacks, and - for Rust - the calls in the arguments of macros and what the
project's own `macro_rules!` expand to.
"""

from __future__ import annotations

import shutil
from pathlib import Path

import callix
import pytest
from callix import RelationKind


def edges(graph: callix.Graph) -> set[tuple[str, str]]:
    return {
        (graph.nodes[r.source_id].qualified_name, graph.nodes[r.target_id].qualified_name)
        for r in graph.relations
        if r.kind is RelationKind.CALLS
    }


needs_go = pytest.mark.skipif(shutil.which("go") is None, reason="the Go toolchain is not installed")
needs_rust = pytest.mark.skipif(
    shutil.which("rust-analyzer") is None or shutil.which("cargo") is None,
    reason="rust-analyzer and cargo are not installed",
)


def go_calls(tmp_path: Path, source: str) -> set[tuple[str, str]]:
    (tmp_path / "go.mod").write_text("module m\n\ngo 1.21\n")
    (tmp_path / "main.go").write_text(source)
    return edges(callix.GoAdapter().analyze(str(tmp_path)))


def rust_calls(tmp_path: Path, source: str) -> set[tuple[str, str]]:
    (tmp_path / "Cargo.toml").write_text('[package]\nname = "m"\nversion = "0.1.0"\nedition = "2021"\n')
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "main.rs").write_text(source)
    return edges(callix.RustAdapter().analyze(str(tmp_path)))


@needs_go
def test_go_range_over_functions_and_variadic_parameters(tmp_path: Path) -> None:
    found = go_calls(
        tmp_path,
        """package main

func one() {}
func two() {}

func run(fns ...func()) {
	for _, fn := range fns {
		fn()
	}
}

func main() {
	run(one, two)
	cs := []func(){one}
	for _, c := range cs {
		c()
	}
}
""",
    )
    assert {("m.run", "m.one"), ("m.run", "m.two"), ("m.main", "m.one")} <= found
    assert ("m.main", "m.two") not in found


@needs_go
def test_go_channel_and_standard_library_callbacks(tmp_path: Path) -> None:
    found = go_calls(
        tmp_path,
        """package main

import (
	"sort"
	"sync"
)

func job() {}
func less(i, j int) bool { return i < j }
func setup() {}

func main() {
	ch := make(chan func(), 1)
	ch <- job
	f := <-ch
	f()
	sort.Slice([]int{2, 1}, less)
	var once sync.Once
	once.Do(setup)
}
""",
    )
    assert {("m.main", "m.job"), ("m.main", "m.less"), ("m.main", "m.setup")} <= found


@needs_go
def test_a_go_function_named_like_a_library_one_is_left_alone(tmp_path: Path) -> None:
    found = go_calls(
        tmp_path,
        """package main

type T struct{}

func (T) Do(f func()) {}

func work() {}

func main() {
	T{}.Do(work)
}
""",
    )
    assert ("m.main", "m.work") not in found


@needs_rust
def test_rust_iterator_and_thread_callbacks_and_for_loops(tmp_path: Path) -> None:
    found = rust_calls(
        tmp_path,
        """use std::thread;

fn double(x: i32) -> i32 { x * 2 }
fn worker() {}
fn one() {}
fn two() {}

fn main() {
    let _ = Some(1).map(double);
    let h = thread::spawn(worker);
    h.join().unwrap();
    let fs: [fn(); 2] = [one, two];
    for f in fs {
        f();
    }
}
""",
    )
    assert {
        ("m::main", "m::double"),
        ("m::main", "m::worker"),
        ("m::main", "m::one"),
        ("m::main", "m::two"),
    } <= found


@needs_rust
def test_rust_calls_in_macro_arguments(tmp_path: Path) -> None:
    found = rust_calls(
        tmp_path,
        """fn compute() -> i32 { 1 }
fn check() -> bool { true }

fn main() {
    println!("{}", compute());
    assert!(check());
    let _ = vec![compute(); 2];
}
""",
    )
    assert {("m::main", "m::compute"), ("m::main", "m::check")} <= found


@needs_rust
def test_rust_expands_the_projects_own_macros(tmp_path: Path) -> None:
    found = rust_calls(
        tmp_path,
        """fn a() {}
fn b() {}
fn setup() {}
fn run(x: i32) {}

macro_rules! call_all {
    ($($f:ident),*) => { $( $f(); )* };
}

macro_rules! launch {
    ($x:expr) => {{
        setup();
        run($x);
    }};
}

fn main() {
    call_all!(a, b);
    launch!(1);
}
""",
    )
    assert {("m::main", "m::a"), ("m::main", "m::b"), ("m::main", "m::setup"), ("m::main", "m::run")} <= found
