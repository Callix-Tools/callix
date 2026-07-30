//! The native callix core: graph models, parsing via tree-sitter, and the
//! language adapters built on top of them.

use pyo3::prelude::*;
use pyo3_stub_gen::define_stub_info_gatherer;

mod boundaries;
mod cfamily;
mod dependencies;
mod diffing;
mod error;
mod golang;
mod graph;
mod ids;
mod metrics;
mod node;
mod occurrence;
mod php;
mod python;
mod relation;
mod roots;
mod rustlang;
mod serde;
mod serialization;
mod span;
mod span_index;
mod status;
mod ts;
mod typescript;
mod yaml;

pub use boundaries::{BoundaryRef, normalize_http_path};
pub use diffing::{GraphDiff, diff_graphs};
pub use error::{
    AdapterError, CallixError, CoreError, DuplicateNodeError, ParseError, ResolverTimeout,
    SerializationError,
};
pub use golang::{GoAdapter, GoResolver};
pub use graph::Graph;
pub use node::{Node, NodeKind};
pub use cfamily::{CAdapter, CFamilyResolver, CppAdapter};
pub use php::{PhpAdapter, PhpResolver};
pub use metrics::ResolverMetrics;
pub use occurrence::{ImportClassifier, OccurrenceRef};
pub use python::{PythonAdapter, ResolvedRef};
pub use python::EmbeddedTyResolver;
pub use relation::{Relation, RelationKind};
pub use rustlang::{RustAdapter, RustResolver};
pub use span::Span;
pub use span_index::SpanIndex;
pub use status::ResolverStatus;
pub use typescript::TsResolver;
pub use yaml::YamlAdapter;

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();

    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("SCHEMA_VERSION", serialization::SCHEMA_VERSION)?;
    m.add("RESOLVER_STATUS_KEY", status::RESOLVER_STATUS_KEY)?;
    m.add("RESOLVER_METRICS_KEY", metrics::RESOLVER_METRICS_KEY)?;

    m.add_class::<Span>()?;
    m.add_class::<NodeKind>()?;
    m.add_class::<Node>()?;
    m.add_class::<RelationKind>()?;
    m.add_class::<Relation>()?;
    m.add_class::<Graph>()?;
    m.add_class::<GraphDiff>()?;
    m.add_class::<SpanIndex>()?;
    m.add_class::<ResolverStatus>()?;
    m.add_class::<OccurrenceRef>()?;
    m.add_class::<ImportClassifier>()?;
    m.add_class::<ResolverMetrics>()?;
    m.add_class::<ResolvedRef>()?;
    m.add_class::<BoundaryRef>()?;
    m.add_class::<PythonAdapter>()?;
    m.add_class::<TsResolver>()?;
    m.add_class::<GoResolver>()?;
    m.add_function(wrap_pyfunction!(golang::is_go_project, m)?)?;
    m.add_function(wrap_pyfunction!(golang::find_go_roots, m)?)?;
    m.add_function(wrap_pyfunction!(golang::go_detect_project_name, m)?)?;
    m.add_function(wrap_pyfunction!(golang::go_read_module_path, m)?)?;
    m.add_function(wrap_pyfunction!(golang::go_parse_dependencies, m)?)?;
    m.add_function(wrap_pyfunction!(golang::classify_go_import, m)?)?;
    m.add_class::<GoAdapter>()?;
    m.add_function(wrap_pyfunction!(golang::extract_go_boundaries, m)?)?;
    m.add_class::<RustResolver>()?;
    m.add_class::<RustAdapter>()?;
    m.add_function(wrap_pyfunction!(rustlang::is_rust_project, m)?)?;
    m.add_function(wrap_pyfunction!(rustlang::find_rust_roots, m)?)?;
    m.add_function(wrap_pyfunction!(rustlang::rust_detect_project_name, m)?)?;
    m.add_function(wrap_pyfunction!(rustlang::rust_read_crate_name, m)?)?;
    m.add_function(wrap_pyfunction!(rustlang::rust_parse_dependencies, m)?)?;
    m.add_function(wrap_pyfunction!(rustlang::classify_rust_import, m)?)?;
    m.add_function(wrap_pyfunction!(rustlang::extract_rust_boundaries, m)?)?;
    m.add_class::<CAdapter>()?;
    m.add_class::<CppAdapter>()?;
    m.add_class::<CFamilyResolver>()?;
    m.add_function(wrap_pyfunction!(cfamily::is_c_project, m)?)?;
    m.add_function(wrap_pyfunction!(cfamily::find_c_roots, m)?)?;
    m.add_function(wrap_pyfunction!(cfamily::c_detect_project_name, m)?)?;
    m.add_function(wrap_pyfunction!(cfamily::is_cpp_project, m)?)?;
    m.add_function(wrap_pyfunction!(cfamily::find_cpp_roots, m)?)?;
    m.add_function(wrap_pyfunction!(cfamily::cpp_detect_project_name, m)?)?;
    m.add_function(wrap_pyfunction!(cfamily::c_parse_dependencies, m)?)?;
    m.add_function(wrap_pyfunction!(cfamily::extract_c_boundaries, m)?)?;
    m.add_function(wrap_pyfunction!(cfamily::extract_cpp_boundaries, m)?)?;
    m.add_class::<PhpAdapter>()?;
    m.add_class::<PhpResolver>()?;
    m.add_function(wrap_pyfunction!(php::is_php_project, m)?)?;
    m.add_function(wrap_pyfunction!(php::find_php_roots, m)?)?;
    m.add_function(wrap_pyfunction!(php::php_detect_project_name, m)?)?;
    m.add_function(wrap_pyfunction!(php::php_parse_dependencies, m)?)?;
    m.add_function(wrap_pyfunction!(php::classify_php_import, m)?)?;
    m.add_function(wrap_pyfunction!(php::extract_php_boundaries, m)?)?;
    m.add_class::<YamlAdapter>()?;
    m.add_function(wrap_pyfunction!(yaml::is_yaml_project, m)?)?;
    m.add_function(wrap_pyfunction!(yaml::find_yaml_roots, m)?)?;
    m.add_function(wrap_pyfunction!(yaml::yaml_detect_project_name, m)?)?;
    m.add_function(wrap_pyfunction!(yaml::extract_yaml_boundaries, m)?)?;
    m.add_class::<typescript::TsImportClassifier>()?;
    m.add_function(wrap_pyfunction!(typescript::visit_typescript_file, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::extract_typescript_boundaries, m)?)?;
    m.add_class::<typescript::TypeScriptAdapter>()?;
    m.add_function(wrap_pyfunction!(typescript::ts_build_root_structure, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::collect_typescript_files_py, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::is_typescript_project, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::find_typescript_roots, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::ts_detect_project_name, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::ts_parse_dependencies, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::ts_get_stdlib_names, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::ts_file_to_qualified_name, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::ts_find_source_roots, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::ts_load_path_aliases, m)?)?;
    m.add_function(wrap_pyfunction!(typescript::ts_resolve_relative_import, m)?)?;
    m.add_class::<EmbeddedTyResolver>()?;

    m.add_function(wrap_pyfunction!(ids::make_node_id, m)?)?;
    m.add_function(wrap_pyfunction!(ids::make_boundary_id, m)?)?;
    m.add_function(wrap_pyfunction!(normalize_http_path, m)?)?;
    m.add_function(wrap_pyfunction!(diff_graphs, m)?)?;
    m.add_function(wrap_pyfunction!(serialization::node_to_dict, m)?)?;
    m.add_function(wrap_pyfunction!(serialization::node_from_dict, m)?)?;
    m.add_function(wrap_pyfunction!(serialization::relation_to_dict, m)?)?;
    m.add_function(wrap_pyfunction!(serialization::relation_from_dict, m)?)?;
    m.add_function(wrap_pyfunction!(serialization::ensure_schema_version, m)?)?;
    m.add_function(wrap_pyfunction!(python::visit_python_file, m)?)?;
    m.add_function(wrap_pyfunction!(python::resolve_relative_import, m)?)?;
    m.add_function(wrap_pyfunction!(python::build_root_structure, m)?)?;
    m.add_function(wrap_pyfunction!(python::apply_resolutions, m)?)?;
    m.add_function(wrap_pyfunction!(python::apply_boundaries, m)?)?;
    m.add_function(wrap_pyfunction!(python::extract_python_boundaries, m)?)?;
    m.add_function(wrap_pyfunction!(python::parse_dependencies, m)?)?;
    m.add_function(wrap_pyfunction!(python::normalize_pkg_name, m)?)?;
    m.add_function(wrap_pyfunction!(python::get_stdlib_names, m)?)?;
    m.add_function(wrap_pyfunction!(python::collect_python_files, m)?)?;
    m.add_function(wrap_pyfunction!(python::filter_nested_files, m)?)?;
    m.add_function(wrap_pyfunction!(python::is_python_project, m)?)?;
    m.add_function(wrap_pyfunction!(python::find_python_roots, m)?)?;
    m.add_function(wrap_pyfunction!(python::detect_project_name, m)?)?;
    m.add_function(wrap_pyfunction!(python::file_to_qualified_name, m)?)?;
    m.add_function(wrap_pyfunction!(python::find_source_roots, m)?)?;
    m.add_function(wrap_pyfunction!(python::is_package_init, m)?)?;

    m.add("CallixError", py.get_type::<CallixError>())?;
    m.add("ParseError", py.get_type::<ParseError>())?;
    m.add("ResolverTimeout", py.get_type::<ResolverTimeout>())?;
    m.add("DuplicateNodeError", py.get_type::<DuplicateNodeError>())?;
    m.add("SerializationError", py.get_type::<SerializationError>())?;
    m.add("AdapterError", py.get_type::<AdapterError>())?;

    Ok(())
}

define_stub_info_gatherer!(stub_info);
