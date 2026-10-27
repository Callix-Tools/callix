//! The bridge from "position in a source file" to "node in the graph".
//!
//! Keeps two lists per file: full node extents and their name spans. The
//! resolution pass hits this once per occurrence, so the lists are sorted by
//! span start up front — a lookup is a binary slice rather than a full scan
//! of the file, as it was in the Python version.

use std::collections::HashMap;
use std::path::Path;

use crate::py::prelude::*;
use crate::py::types::PyDict;
use crate::py::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::graph::Graph;
use crate::node::NodeKind;
use crate::relation::RelationKind;
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

/// The spans of a node's `declared_at` list.
#[cfg(feature = "python")]
fn declared_spans(value: &Bound<'_, PyAny>) -> Vec<Span> {
    value.extract::<Vec<Span>>().unwrap_or_default()
}

#[cfg(not(feature = "python"))]
fn declared_spans(value: &Bound<'_, PyAny>) -> Vec<Span> {
    match value.value() {
        crate::py::Value::List(items) => items
            .iter()
            .filter_map(|item| match item {
                crate::py::Value::Span(span) => Some(*span),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
#[derive(Default)]
pub struct SpanIndex {
    full: Table,
    name: Table,
    /// File path -> id of the MODULE that file belongs to, for the MODULE nodes
    /// that are placed (Python and TypeScript). Kept apart from the two tables so
    /// that a module's zero-width placeholder never competes with a real name.
    file_modules: HashMap<String, String>,
}

/// A zero-width span at the very start of a file: where a MODULE sits.
fn is_module_placeholder(span: &Span) -> bool {
    span.start_line == 1 && span.start_col == 1 && span.end_line == 1 && span.end_col == 1
}

/// `file` relative to `primary`, else to `fallback`, else as it is. The path a
/// FILE node stores.
pub(crate) fn relative_file_path(file: &Path, primary: &Path, fallback: &Path) -> String {
    file.strip_prefix(primary)
        .or_else(|_| file.strip_prefix(fallback))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.to_string_lossy().into_owned())
}

/// `(file_path, span, metadata)` for a new MODULE node. A module with a file of
/// its own sits at a zero-width 1:1 span in that file, and its `name_span` is the
/// same; a module without one (a namespace package, a directory with no index
/// file) stays unplaced.
pub(crate) fn module_placement(
    py: Python<'_>,
    file: Option<&String>,
) -> PyResult<(Option<String>, Option<Span>, Py<PyDict>)> {
    let metadata = PyDict::new(py);
    let Some(file) = file else {
        return Ok((None, None, metadata.unbind()));
    };
    let at = Span { start_line: 1, start_col: 1, end_line: 1, end_col: 1 };
    metadata.set_item("name_span", at)?;
    Ok((Some(file.clone()), Some(at), metadata.unbind()))
}

impl SpanIndex {
    /// Lookup by name span — the same table `at()` uses.
    pub(crate) fn lookup_name(&mut self, file_path: &str, line: u32, col: u32) -> Option<String> {
        self.name.smallest_containing(file_path, line, col)
    }

    /// The MODULE a file belongs to, when the position is the start of the file
    /// (1:1) — where a resolver answers a reference to a module. Callers try
    /// `lookup_name` first, so a real name at 1:1 always wins.
    pub(crate) fn lookup_module(&self, file_path: &str, line: u32, col: u32) -> Option<String> {
        if (line, col) != (1, 1) {
            return None;
        }
        self.file_modules.get(file_path).cloned()
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
            // A placed MODULE is a zero-width placeholder at 1:1; indexed, it would
            // beat `x = 1` at 1:1 (smallest area) and steal real references.
            if node.kind == NodeKind::Module && is_module_placeholder(&span) {
                continue;
            }
            idx.full.add(file_path.clone(), node.id.clone(), span);

            let metadata = node.metadata.bind(py);
            if let Some(value) = metadata.get_item("name_span")?
                && let Ok(name_span) = value.extract::<Span>()
            {
                idx.name.add(file_path.clone(), node.id.clone(), name_span);
            }
            // Other places the definition is declared - the signatures an
            // overloaded function is resolved to.
            if let Some(value) = metadata.get_item("declared_at")? {
                for span in declared_spans(&value) {
                    idx.name.add(file_path.clone(), node.id.clone(), span);
                }
            }
        }
        // Every file of a placed module (TypeScript can have several per module).
        let nodes = graph.node_map();
        for relation in graph.relation_slice() {
            let relation = relation.get();
            if relation.kind != RelationKind::Contains {
                continue;
            }
            let (Some(source), Some(target)) =
                (nodes.get(&relation.source_id), nodes.get(&relation.target_id))
            else {
                continue;
            };
            let (source, target) = (source.get(), target.get());
            if source.kind == NodeKind::Module
                && source.span.is_some()
                && target.kind == NodeKind::File
                && let Some(path) = target.file_path.as_ref()
            {
                idx.file_modules.entry(path.clone()).or_insert_with(|| source.id.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn span(line: u32, col: u32, end_col: u32) -> Span {
        Span { start_line: line, start_col: col, end_line: line, end_col }
    }

    #[test]
    fn a_module_is_found_at_the_start_of_its_file_only() {
        let mut idx = SpanIndex::default();
        idx.file_modules.insert("pkg/util.py".into(), "m".into());
        assert_eq!(idx.lookup_module("pkg/util.py", 1, 1).as_deref(), Some("m"));
        assert_eq!(idx.lookup_module("pkg/util.py", 1, 2), None);
        assert_eq!(idx.lookup_module("pkg/util.py", 2, 1), None);
        assert_eq!(idx.lookup_module("other.py", 1, 1), None);
    }

    #[test]
    fn a_name_at_the_start_of_the_file_wins_over_its_module() {
        let mut idx = SpanIndex::default();
        idx.file_modules.insert("pkg/util.py".into(), "m".into());
        idx.add_name("pkg/util.py".into(), "v".into(), span(1, 1, 2));
        let found = idx
            .lookup_name("pkg/util.py", 1, 1)
            .or_else(|| idx.lookup_module("pkg/util.py", 1, 1));
        assert_eq!(found.as_deref(), Some("v"));
        // With no name there, the module answers.
        let found = idx
            .lookup_name("pkg/other.py", 1, 1)
            .or_else(|| idx.lookup_module("pkg/util.py", 1, 1));
        assert_eq!(found.as_deref(), Some("m"));
    }

    #[test]
    fn every_file_of_a_module_answers() {
        let mut idx = SpanIndex::default();
        idx.file_modules.insert("foo.ts".into(), "m".into());
        idx.file_modules.insert("foo.d.ts".into(), "m".into());
        assert_eq!(idx.lookup_module("foo.d.ts", 1, 1).as_deref(), Some("m"));
        assert_eq!(idx.lookup_module("foo.ts", 1, 1).as_deref(), Some("m"));
    }

    #[test]
    fn relative_file_path_strips_the_primary_root_then_the_fallback() {
        let p = Path::new;
        assert_eq!(relative_file_path(p("/r/a/x.py"), p("/r"), p("/r/a")), "a/x.py");
        assert_eq!(relative_file_path(p("/q/x.py"), p("/r"), p("/q")), "x.py");
        assert_eq!(relative_file_path(p("/z/x.py"), p("/r"), p("/q")), "/z/x.py");
    }
}
