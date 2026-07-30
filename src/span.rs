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

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start_line: u32, start_col: u32, end_line: u32, end_col: u32) -> Span {
        Span { start_line, start_col, end_line, end_col }
    }

    /// Every coordinate is 1-based, so line/column 0 is never a valid
    /// position inside a span the visitors produce.
    #[test]
    fn coordinates_are_one_based() {
        let s = span(1, 1, 1, 5);
        assert!(!s.contains(0, 1));
        assert!(!s.contains(1, 0));
        assert!(s.contains(1, 1));
    }

    #[test]
    fn contains_is_inclusive_at_both_ends() {
        let s = span(3, 4, 3, 9);
        assert!(s.contains(3, 4), "the exact start is inside");
        assert!(s.contains(3, 9), "the exact end is inside");
        assert!(!s.contains(3, 3), "one column before is outside");
        assert!(!s.contains(3, 10), "one column after is outside");
    }

    #[test]
    fn single_line_span_rejects_neighbouring_lines() {
        let s = span(5, 2, 5, 8);
        assert!(s.contains(5, 5));
        assert!(!s.contains(4, 5));
        assert!(!s.contains(6, 5));
        // A column outside the range on a neighbouring line is still out.
        assert!(!s.contains(4, 100));
        assert!(!s.contains(6, 1));
    }

    /// On a multi-line span the column only constrains the first and last
    /// lines: a position in the middle is inside whatever its column.
    #[test]
    fn multi_line_span_only_constrains_the_edge_lines() {
        let s = span(2, 10, 6, 4);
        assert!(s.contains(2, 10));
        assert!(!s.contains(2, 9), "before the start column on the first line");
        assert!(s.contains(3, 0), "any column on an interior line");
        assert!(s.contains(4, 999));
        assert!(s.contains(6, 4), "the exact end");
        assert!(!s.contains(6, 5), "past end_col on the last line");
        assert!(!s.contains(7, 0));
        assert!(!s.contains(1, 999));
    }

    #[test]
    fn empty_span_contains_only_its_own_position() {
        let s = span(4, 7, 4, 7);
        assert!(s.contains(4, 7));
        assert!(!s.contains(4, 6));
        assert!(!s.contains(4, 8));
    }

    /// `area` orders spans from tightest to widest; the line difference wins
    /// over the column difference, so a wide one-liner still beats a short
    /// multi-liner.
    #[test]
    fn area_ranks_tighter_spans_lower() {
        assert_eq!(span(3, 4, 3, 9).area(), (0, 5));
        assert_eq!(span(2, 1, 8, 3).area(), (6, 2));
        assert!(span(1, 0, 1, 100).area() < span(1, 0, 2, 1).area());
        assert!(span(3, 4, 3, 5).area() < span(3, 4, 3, 9).area());
    }

    /// A span whose end column is smaller than its start column happens
    /// whenever it covers more than one line; the saturating subtraction
    /// keeps `area` from wrapping around.
    #[test]
    fn area_saturates_instead_of_wrapping() {
        assert_eq!(span(1, 40, 3, 2).area(), (2, 0));
    }

    #[test]
    fn spans_are_compared_by_value() {
        assert_eq!(span(1, 2, 3, 4), span(1, 2, 3, 4));
        assert_ne!(span(1, 2, 3, 4), span(1, 2, 3, 5));
    }
}
