//! A range in a source file. Every value is 1-based (tree-sitter reports
//! 0-based — convert at the visitor boundary).

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

#[gen_stub_pyclass]
#[pyclass(module = "callix._core", frozen, eq, hash, get_all, from_py_object)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Span {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

#[gen_stub_pymethods]
#[pymethods]
impl Span {
    #[new]
    fn new(start_line: u32, start_col: u32, end_line: u32, end_col: u32) -> Self {
        Self { start_line, start_col, end_line, end_col }
    }

    /// Whether the range contains the (1-based) position.
    #[pyo3(name = "contains")]
    fn py_contains(&self, line: u32, col: u32) -> bool {
        self.contains(line, col)
    }

    fn __repr__(&self) -> String {
        format!(
            "Span({}:{}-{}:{})",
            self.start_line, self.start_col, self.end_line, self.end_col
        )
    }
}

impl Span {
    pub fn contains(&self, line: u32, col: u32) -> bool {
        (line, col) >= (self.start_line, self.start_col)
            && (line, col) <= (self.end_line, self.end_col)
    }

    /// How "narrow" the range is: smaller is tighter. Used to pick the
    /// innermost node.
    pub fn area(&self) -> (u32, u32) {
        (
            self.end_line.saturating_sub(self.start_line),
            self.end_col.saturating_sub(self.start_col),
        )
    }
}
