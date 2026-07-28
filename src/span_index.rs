//! The bridge from "position in a source file" to "node in the graph".
//!
//! Keeps two lists per file: full node extents and their name spans. The
//! resolution pass hits this once per occurrence, so the lists are sorted by
//! span start up front — a lookup is a binary slice rather than a full scan
//! of the file, as it was in the Python version.

use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::graph::Graph;
use crate::span::Span;

type Entry = (String, Span);

#[derive(Default)]
struct Table {
    per_file: HashMap<String, Vec<Entry>>,
    sorted: bool,
}

impl Table {
    fn add(&mut self, file_path: String, node_id: String, span: Span) {
        self.per_file.entry(file_path).or_default().push((node_id, span));
        self.sorted = false;
    }

    fn sort(&mut self) {
        if self.sorted {
            return;
        }
        for entries in self.per_file.values_mut() {
            entries.sort_by_key(|(_, s)| (s.start_line, s.start_col));
        }
        self.sorted = true;
    }

    /// The tightest span containing the position.
    fn smallest_containing(&mut self, file_path: &str, line: u32, col: u32) -> Option<String> {
        self.sort();
        let entries = self.per_file.get(file_path)?;

        // Spans are sorted by start, so once a start lies strictly to the
        // right of the position, nothing after it can contain the position.
        let end = entries.partition_point(|(_, s)| (s.start_line, s.start_col) <= (line, col));

        entries[..end]
            .iter()
            .filter(|(_, s)| s.contains(line, col))
            .min_by_key(|(_, s)| s.area())
            .map(|(id, _)| id.clone())
    }
}

#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
#[derive(Default)]
pub struct SpanIndex {
    full: Table,
    name: Table,
}

impl SpanIndex {
    /// Lookup by name span — the same table `at()` uses.
    pub(crate) fn lookup_name(&mut self, file_path: &str, line: u32, col: u32) -> Option<String> {
        self.name.smallest_containing(file_path, line, col)
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl SpanIndex {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    /// Registers a node's full extent.
    fn add_full(&mut self, file_path: String, node_id: String, span: Span) {
        self.full.add(file_path, node_id, span);
    }

    /// Registers a node's name (identifier) span.
    fn add_name(&mut self, file_path: String, node_id: String, name_span: Span) {
        self.name.add(file_path, node_id, name_span);
    }

    /// Builds the index over every graph node that carries a span.
    #[staticmethod]
    pub(crate) fn from_graph(py: Python<'_>, graph: &Graph) -> PyResult<Self> {
        let mut idx = Self::default();
        for node in graph.node_map().values() {
            let node = node.get();
            let (Some(file_path), Some(span)) = (node.file_path.as_ref(), node.span) else {
                continue;
            };
            idx.full.add(file_path.clone(), node.id.clone(), span);

            let metadata = node.metadata.bind(py);
            if let Some(value) = metadata.get_item("name_span")?
                && let Ok(name_span) = value.extract::<Span>()
            {
                idx.name.add(file_path.clone(), node.id.clone(), name_span);
            }
        }
        Ok(idx)
    }

    /// The innermost node whose full extent contains the (1-based) position.
    fn enclosing(&mut self, file_path: &str, line: u32, col: u32) -> Option<String> {
        self.full.smallest_containing(file_path, line, col)
    }

    /// The node whose name span contains the (1-based) position.
    fn at(&mut self, file_path: &str, line: u32, col: u32) -> Option<String> {
        self.name.smallest_containing(file_path, line, col)
    }
}
