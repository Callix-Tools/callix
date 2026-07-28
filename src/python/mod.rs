//! Python-адаптер: разбор через tree-sitter и построение структуры графа.

mod adapter;
mod boundary;
mod deps;
mod detector;
mod module_resolver;
mod ty_embedded;
mod visitor;

use std::cell::RefCell;

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use tree_sitter::{Parser, Tree};

use crate::error::ParseError;
use crate::graph::Graph;
use crate::occurrence::{ImportClassifier, OccurrenceRef};

pub(crate) use adapter::{
    add_boundary_rust, apply_boundaries_rust, apply_resolutions_rust, innermost_enclosing_rust,
};
pub use adapter::{
    PythonAdapter, ResolvedRef, apply_boundaries, apply_resolutions, build_root_structure,
    collect_python_files, filter_nested_files,
};
pub use boundary::extract_python_boundaries;
pub use deps::{get_stdlib_names, normalize_pkg_name, parse_dependencies};
pub use detector::{detect_project_name, find_python_roots, is_python_project};
pub use module_resolver::{file_to_qualified_name, find_source_roots, is_package_init};
pub use ty_embedded::EmbeddedTyResolver;
pub use visitor::PythonVisitor;

thread_local! {
    /// Парсер переиспользуется в пределах потока: настройка грамматики
    /// заметно дороже самого разбора файла.
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("грамматика python совместима с этой версией tree-sitter");
        parser
    });
}

pub fn parse_tree(source: &[u8]) -> Result<Tree, PyErr> {
    PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .ok_or_else(|| ParseError::new_err("tree-sitter не смог разобрать исходник"))
}

/// Приводит относительный импорт к абсолютному имени.
///
/// `level` — число ведущих точек (1 — текущий пакет, 2 — родительский).
/// Пакет текущего модуля — это все части имени, кроме последней.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (current_module_qname, level, module = None))]
pub fn resolve_relative_import(
    current_module_qname: &str,
    level: usize,
    module: Option<&str>,
) -> String {
    let parts: Vec<&str> = current_module_qname.split('.').collect();
    let base = &parts[..parts.len().saturating_sub(level)];

    match module {
        Some(module) if !module.is_empty() => {
            if base.is_empty() {
                module.to_string()
            } else {
                format!("{}.{}", base.join("."), module)
            }
        }
        _ => base.join("."),
    }
}

/// Разбирает один файл и наполняет граф его структурой.
///
/// Рёбра CALLS/REFERENCES/HAS_TYPE/INHERITS_FROM здесь не создаются —
/// вместо них возвращается список мест использования, который резолв-фаза
/// на стороне Python привяжет к определениям.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (graph, project_name, file_path, module_qualified_name, file_node_id, source, classifier = None))]
#[allow(clippy::too_many_arguments)]
pub fn visit_python_file(
    py: Python<'_>,
    graph: &Bound<'_, Graph>,
    project_name: &str,
    file_path: &str,
    module_qualified_name: &str,
    file_node_id: &str,
    source: &Bound<'_, PyBytes>,
    classifier: Option<&ImportClassifier>,
) -> PyResult<Vec<OccurrenceRef>> {
    let source = source.as_bytes();
    let tree = parse_tree(source)?;

    let default_classifier = ImportClassifier::default();
    let classifier = classifier.unwrap_or(&default_classifier);

    let mut graph = graph.borrow_mut();
    let mut visitor = PythonVisitor::new(
        py,
        &mut graph,
        source,
        project_name,
        file_path,
        module_qualified_name,
        file_node_id,
        classifier,
    );
    visitor.visit(tree.root_node())?;
    Ok(visitor.into_occurrences())
}
