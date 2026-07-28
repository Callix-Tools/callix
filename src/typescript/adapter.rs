//! The TypeScript adapter's structural phase and analysis orchestration.
//!
//! Laid out like the Python adapter: structure per root, then a single
//! resolution pass for the whole call, then boundaries.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pyfunction, gen_stub_pymethods};

use crate::boundaries::BoundaryRef;
use crate::error::AdapterError;
use crate::graph::Graph;
use crate::ids::node_id;
use crate::metrics::{RESOLVER_METRICS_KEY, ResolverMetrics};
use crate::node::{Node, NodeKind};
use crate::occurrence::OccurrenceRef;
use crate::python::ResolvedRef;
use crate::relation::RelationKind;
use crate::roots::{collect_files, filter_nested_root_files};
use crate::span_index::SpanIndex;
use crate::status::{RESOLVER_STATUS_KEY, ResolverStatus};

use super::boundary::extract_boundaries;
use super::detector::{TS_EXCLUDED_DIRS, dependencies, node_builtins, project_name, typescript_roots};
use super::module_resolver::{path_aliases, qualified_name, source_roots};
use super::resolver::TsResolver;
use super::visitor::{TsImportClassifier, TsVisitor};

const TS_EXTENSIONS: [&str; 4] = [".ts", ".tsx", ".mts", ".cts"];

/// Declaration files carry types only, no implementation.
const DECLARATION_SUFFIXES: [&str; 3] = [".d.ts", ".d.mts", ".d.cts"];

type FileBoundaries = (String, String, Vec<BoundaryRef>);
type BuiltRoot = (String, Vec<(String, OccurrenceRef)>, Vec<FileBoundaries>);

fn is_declaration(path: &Path) -> bool {
    let name = path.to_string_lossy();
    DECLARATION_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

/// TypeScript sources under the root, declaration files excluded.
pub fn collect_typescript_files(root: &Path) -> Vec<PathBuf> {
    collect_files(root, &TS_EXTENSIONS, &TS_EXCLUDED_DIRS)
        .into_iter()
        .filter(|p| !is_declaration(p))
        .collect()
}

/// Structure and imports for one TypeScript project root.
fn build_root_structure(
    py: Python<'_>,
    graph: &mut Graph,
    project_root: &Path,
    lang_root: &Path,
    files: &[PathBuf],
) -> PyResult<BuiltRoot> {
    let project = project_name(lang_root);
    let roots = source_roots(lang_root, files);

    let mut internal_tops: HashSet<String> = HashSet::new();
    for file in files {
        let root = roots.iter().find(|r| file.starts_with(r)).unwrap_or(&roots[0]);
        if let Some(qname) = qualified_name(file, root) {
            internal_tops.insert(qname.split('.').next().unwrap_or(&qname).to_string());
        }
    }

    let third_party = dependencies(lang_root);
    // Root-level configs (next.config.ts and friends) push names like "next"
    // into internal_tops; when a name is declared in package.json, the
    // manifest wins.
    internal_tops.retain(|name| !third_party.contains(name));

    let aliases = path_aliases(lang_root);
    let classifier = TsImportClassifier::from_sets(node_builtins(), third_party, internal_tops);

    let project_id = node_id(&project, &project, NodeKind::Project.as_str());
    if !graph.has_node(&project_id) {
        let node = Node {
            id: project_id.clone(),
            kind: NodeKind::Project,
            qualified_name: project.clone(),
            name: project.clone(),
            file_path: None,
            span: None,
            metadata: PyDict::new(py).unbind(),
        };
        graph.insert_node(project_id.clone(), Py::new(py, node)?);
    }

    let mut modules: IndexMap<String, String> = IndexMap::new();
    let mut all_occurrences: Vec<(String, OccurrenceRef)> = Vec::new();
    let mut all_boundaries: Vec<FileBoundaries> = Vec::new();

    for file in files {
        let source_root = roots.iter().find(|r| file.starts_with(r)).unwrap_or(&roots[0]);
        let Some(module_qname) = qualified_name(file, source_root) else {
            continue;
        };
        let leaf_module_id =
            ensure_module_chain(py, graph, &project, &module_qname, &mut modules)?;

        let relative_path = file
            .strip_prefix(project_root)
            .or_else(|_| file.strip_prefix(lang_root))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| file.to_string_lossy().into_owned());

        let file_id = node_id(&project, &relative_path, NodeKind::File.as_str());
        if !graph.has_node(&file_id) {
            let node = Node {
                id: file_id.clone(),
                kind: NodeKind::File,
                qualified_name: relative_path.clone(),
                name: file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                file_path: Some(relative_path),
                span: None,
                metadata: PyDict::new(py).unbind(),
            };
            graph.insert_node(file_id.clone(), Py::new(py, node)?);
            push_relation(py, graph, leaf_module_id, file_id.clone(), RelationKind::Contains)?;
        }

        let Ok(source) = std::fs::read(file) else {
            continue;
        };
        let tsx = file
            .extension()
            .is_some_and(|e| e.to_string_lossy().to_lowercase() == "tsx");
        let tree = super::parse_tree(&source, tsx)?;

        let abs_path = file.to_string_lossy().into_owned();
        let source_root_name = source_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut visitor = TsVisitor::new(
            py,
            graph,
            &source,
            &project,
            &abs_path,
            &module_qname,
            &file_id,
            source_root_name,
            &classifier,
            &modules,
            &aliases,
        );
        visitor.visit(tree.root_node())?;
        for occurrence in visitor.into_occurrences() {
            all_occurrences.push((abs_path.clone(), occurrence));
        }

        let boundaries = extract_boundaries(tree.root_node(), &source, tsx);
        if !boundaries.is_empty() {
            all_boundaries.push((abs_path, file_id, boundaries));
        }
    }

    let top_level: Vec<String> = modules
        .iter()
        .filter(|(qname, _)| !qname.contains('.'))
        .map(|(_, id)| id.clone())
        .collect();
    for module_id in top_level {
        push_relation(py, graph, project_id.clone(), module_id, RelationKind::Contains)?;
    }

    Ok((project, all_occurrences, all_boundaries))
}

fn ensure_module_chain(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    module_qname: &str,
    modules: &mut IndexMap<String, String>,
) -> PyResult<String> {
    let parts: Vec<&str> = module_qname.split('.').collect();
    let mut parent_id: Option<String> = None;

    for i in 1..=parts.len() {
        let qname = parts[..i].join(".");
        if !modules.contains_key(&qname) {
            let id = node_id(project, &qname, NodeKind::Module.as_str());
            let node = Node {
                id: id.clone(),
                kind: NodeKind::Module,
                qualified_name: qname.clone(),
                name: parts[i - 1].to_string(),
                file_path: None,
                span: None,
                metadata: PyDict::new(py).unbind(),
            };
            graph.insert_node(id.clone(), Py::new(py, node)?);
            modules.insert(qname.clone(), id.clone());
            if let Some(parent_id) = &parent_id {
                push_relation(py, graph, parent_id.clone(), id, RelationKind::Contains)?;
            }
        }
        parent_id = modules.get(&qname).cloned();
    }
    Ok(modules[module_qname].clone())
}

fn push_relation(
    py: Python<'_>,
    graph: &mut Graph,
    source_id: String,
    target_id: String,
    kind: RelationKind,
) -> PyResult<()> {
    let relation = crate::relation::Relation {
        source_id,
        target_id,
        kind,
        metadata: PyDict::new(py).unbind(),
    };
    graph.push_relation(Py::new(py, relation)?);
    Ok(())
}

/// The language adapter for TypeScript projects.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core", unsendable)]
pub struct TypeScriptAdapter {
    resolver: Option<TsResolver>,
}

#[gen_stub_pymethods]
#[pymethods]
impl TypeScriptAdapter {
    /// Args:
    ///     resolve: False turns the resolution phase off — the graph stays
    ///         structural and `resolver_status` becomes `unavailable`.
    #[new]
    #[pyo3(signature = (*, resolve = true))]
    fn new(resolve: bool) -> Self {
        Self {
            resolver: resolve.then(TsResolver::empty),
        }
    }

    fn language(&self) -> &'static str {
        "typescript"
    }

    fn file_extensions(&self) -> HashSet<String> {
        TS_EXTENSIONS.iter().map(|e| (*e).to_string()).collect()
    }

    fn can_handle(&self, project_root: PathBuf) -> bool {
        super::detector::is_typescript(&project_root)
    }

    /// Sources under the root, declaration files excluded.
    fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_typescript_files(&project_root)
    }

    /// Analyses the project and returns the graph.
    #[pyo3(signature = (project_root, files = None, *, strict = false))]
    fn analyze(
        &mut self,
        py: Python<'_>,
        project_root: PathBuf,
        files: Option<Vec<PathBuf>>,
        strict: bool,
    ) -> PyResult<Graph> {
        let project_root = project_root.canonicalize().unwrap_or(project_root);

        let root_files: Vec<(PathBuf, Vec<PathBuf>)> = match files {
            Some(files) => vec![(project_root.clone(), files)],
            None => {
                let roots = typescript_roots(&project_root);
                roots
                    .iter()
                    .map(|lang_root| {
                        let collected = collect_typescript_files(lang_root);
                        let kept = filter_nested_root_files(collected, lang_root, &roots);
                        (lang_root.clone(), kept)
                    })
                    .collect()
            }
        };

        let mut graph = Graph::empty(py);

        // Phase 1 — structure per root, no resolution.
        let mut built = Vec::with_capacity(root_files.len());
        for (lang_root, file_list) in &root_files {
            built.push(build_root_structure(
                py,
                &mut graph,
                &project_root,
                lang_root,
                file_list,
            )?);
        }

        // Phase 2 — one resolver for the whole project_root.
        let mut metrics = ResolverMetrics::default();
        let mut status = ResolverStatus::Unavailable;
        if let Some(resolver) = &mut self.resolver {
            resolver.prepare_rust(&project_root)?;
            let mut span_index = SpanIndex::from_graph(py, &graph)?;
            for (project, occurrences, _boundaries) in &built {
                let pass = resolve_pass(
                    py,
                    resolver,
                    &mut graph,
                    &mut span_index,
                    project,
                    &project_root,
                    occurrences,
                )?;
                metrics.merge(&pass);
            }
            status = resolver.status_rust();
        }

        // Phase 3 — boundaries, after resolution.
        for (_project, _occurrences, boundaries) in &built {
            crate::python::apply_boundaries_rust(py, &mut graph, boundaries)?;
        }

        let status_value = status.as_str().into_pyobject(py)?;
        graph.set_metadata_item(py, RESOLVER_STATUS_KEY, status_value.as_any())?;
        graph.set_metadata_item(py, RESOLVER_METRICS_KEY, metrics.as_dict(py)?.as_any())?;

        if strict && status != ResolverStatus::Ok {
            return Err(AdapterError::new_err(format!(
                "TypeScript resolver status is '{}'; refusing to return a degraded graph in strict mode",
                status.as_str()
            )));
        }
        Ok(graph)
    }

    fn __repr__(&self) -> String {
        format!(
            "TypeScriptAdapter(resolver={})",
            if self.resolver.is_some() { "tsgo" } else { "off" }
        )
    }
}

fn resolve_pass(
    py: Python<'_>,
    resolver: &TsResolver,
    graph: &mut Graph,
    span_index: &mut SpanIndex,
    project: &str,
    project_root: &Path,
    occurrences: &[(String, OccurrenceRef)],
) -> PyResult<ResolverMetrics> {
    if occurrences.is_empty() {
        return Ok(ResolverMetrics::default());
    }
    let started = Instant::now();
    let refs: Vec<Option<ResolvedRef>> = occurrences
        .iter()
        .map(|(path, occurrence)| resolver.resolve_rust(path, occurrence.line, occurrence.col))
        .collect();
    let elapsed = started.elapsed().as_secs_f64();

    let mut metrics = crate::python::apply_resolutions_rust(
        py,
        graph,
        project,
        project_root,
        span_index,
        occurrences,
        &refs,
    )?;
    metrics.seconds = elapsed;
    Ok(metrics)
}

/// One root's structural phase — the entry point for checks.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (graph, project_root, lang_root, files))]
pub fn ts_build_root_structure(
    py: Python<'_>,
    graph: &Bound<'_, Graph>,
    project_root: PathBuf,
    lang_root: PathBuf,
    files: Vec<PathBuf>,
) -> PyResult<BuiltRoot> {
    let mut graph = graph.borrow_mut();
    build_root_structure(py, &mut graph, &project_root, &lang_root, &files)
}

/// Every TypeScript file under the root, declarations excluded.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn collect_typescript_files_py(project_root: PathBuf) -> Vec<PathBuf> {
    collect_typescript_files(&project_root)
}
