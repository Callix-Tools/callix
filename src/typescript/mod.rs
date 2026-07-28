//! TypeScript-адаптер. Пока здесь только резолвер символов поверх
//! typescript-go, слинкованного в модуль (см. `go/bridge.go` и `build.rs`).

mod adapter;
mod boundary;
mod detector;
mod module_resolver;
mod resolver;
mod visitor;

pub use detector::{
    find_typescript_roots, is_typescript_project, ts_detect_project_name, ts_get_stdlib_names,
    ts_parse_dependencies,
};
pub use module_resolver::{
    ts_file_to_qualified_name, ts_find_source_roots, ts_load_path_aliases,
    ts_resolve_relative_import,
};
pub use adapter::{TypeScriptAdapter, collect_typescript_files_py, ts_build_root_structure};
pub use boundary::extract_typescript_boundaries;
pub use resolver::TsResolver;
pub use visitor::{TsImportClassifier, TsVisitor};

use std::cell::RefCell;
use std::collections::BTreeMap;

use indexmap::IndexMap;

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use tree_sitter::{Parser, Tree};

use crate::error::ParseError;
use crate::graph::Graph;
use crate::occurrence::OccurrenceRef;

thread_local! {
    /// По парсеру на грамматику: у .ts и .tsx они разные.
    static TS_PARSER: RefCell<Parser> = RefCell::new(parser_for(false));
    static TSX_PARSER: RefCell<Parser> = RefCell::new(parser_for(true));
}

fn parser_for(tsx: bool) -> Parser {
    let mut parser = Parser::new();
    let language = if tsx {
        tree_sitter_typescript::LANGUAGE_TSX
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT
    };
    parser
        .set_language(&language.into())
        .expect("грамматика typescript совместима с этой версией tree-sitter");
    parser
}

pub fn parse_tree(source: &[u8], tsx: bool) -> Result<Tree, PyErr> {
    let parsed = if tsx {
        TSX_PARSER.with(|p| p.borrow_mut().parse(source, None))
    } else {
        TS_PARSER.with(|p| p.borrow_mut().parse(source, None))
    };
    parsed.ok_or_else(|| ParseError::new_err("tree-sitter не смог разобрать исходник"))
}

/// Разбирает один TypeScript-файл и наполняет граф его структурой.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (graph, project_name, file_path, module_qualified_name, file_node_id, source, source_root_name, classifier = None, modules = None, path_aliases = None, tsx = false))]
#[allow(clippy::too_many_arguments)]
pub fn visit_typescript_file(
    py: Python<'_>,
    graph: &Bound<'_, Graph>,
    project_name: &str,
    file_path: &str,
    module_qualified_name: &str,
    file_node_id: &str,
    source: &Bound<'_, PyBytes>,
    source_root_name: String,
    classifier: Option<&TsImportClassifier>,
    modules: Option<IndexMap<String, String>>,
    path_aliases: Option<BTreeMap<String, String>>,
    tsx: bool,
) -> PyResult<Vec<OccurrenceRef>> {
    let source = source.as_bytes();
    let tree = parse_tree(source, tsx)?;

    let default_classifier = TsImportClassifier::default();
    let classifier = classifier.unwrap_or(&default_classifier);
    let modules = modules.unwrap_or_default();
    let path_aliases = path_aliases.unwrap_or_default();

    let mut graph = graph.borrow_mut();
    let mut visitor = TsVisitor::new(
        py,
        &mut graph,
        source,
        project_name,
        file_path,
        module_qualified_name,
        file_node_id,
        source_root_name,
        classifier,
        &modules,
        &path_aliases,
    );
    visitor.visit(tree.root_node())?;
    Ok(visitor.into_occurrences())
}
