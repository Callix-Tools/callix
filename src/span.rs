//! Диапазон в исходнике. Все значения 1-based (tree-sitter отдаёт 0-based —
//! конвертировать на границе визитора).

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

    /// Содержит ли диапазон позицию (1-based).
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

    /// Насколько диапазон «узкий»: меньше — теснее. Для выбора внутреннего узла.
    pub fn area(&self) -> (u32, u32) {
        (
            self.end_line.saturating_sub(self.start_line),
            self.end_col.saturating_sub(self.start_col),
        )
    }
}
