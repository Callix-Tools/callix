//! Orchestration of PHP project analysis.
//!
//! A MODULE here is a namespace, and the separator is a backslash at every
//! level — `App\Model` contains `App\Model\User`, and PHP writes `::` only in
//! expressions. Namespaces nest, so the MODULE chain is built segment by
//! segment the way the Python adapter builds a dotted one, while a Go package
//! has no parent to hang off.
//!
//! The three phases run in the order the other adapters use, and for the same
//! reasons: structure for every root first, then ONE resolver for the whole
//! call, then boundaries.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::boundaries::{BoundaryRef, run_extractors};
use crate::dependencies::declare_dependencies;
use crate::error::AdapterError;
use crate::graph::Graph;
use crate::ids::node_id;
use crate::metrics::{RESOLVER_METRICS_KEY, ResolverMetrics};
use crate::node::{Node, NodeKind};
use crate::occurrence::OccurrenceRef;
use crate::python::ResolvedRef;
use crate::relation::{Relation, RelationKind};
use crate::roots::{EXCLUDED_DIRS, collect_files, filter_nested_root_files};
use crate::span_index::SpanIndex;
use crate::status::{RESOLVER_STATUS_KEY, ResolverStatus};

use super::boundary::extract_boundaries;
use super::deps::{dependencies, internal_prefixes};
use super::detector::{
    PHP_EXCLUDED_AT_ROOT, PHP_EXCLUDED_DIRS, PHP_EXTENSIONS, is_php, php_roots, project_name,
};
use super::resolver::PhpResolver;
use super::visitor::{PhpVisitor, has_php_statements, php_namespace};

type FileBoundaries = (String, String, Vec<BoundaryRef>);
type BuiltRoot = (String, Vec<(String, OccurrenceRef)>, Vec<FileBoundaries>);

/// PHP sources under the root.
///
/// `vendor/` is matched at the root only, where Composer writes it: a project
/// may legitimately hold `src/Vendor/`, and dropping the third-party tree must
/// not take first-party code with it.
pub fn collect_php_files(root: &Path) -> Vec<PathBuf> {
    let excluded: Vec<&str> = EXCLUDED_DIRS
        .iter()
        .chain(PHP_EXCLUDED_DIRS.iter())
        .copied()
        .collect();
    collect_files(root, &PHP_EXTENSIONS, &excluded, &PHP_EXCLUDED_AT_ROOT)
}

fn push_relation(
    py: Python<'_>,
    graph: &mut Graph,
    source_id: String,
    target_id: String,
    kind: RelationKind,
) -> PyResult<()> {
    let relation = Relation {
        source_id,
        target_id,
        kind,
        metadata: PyDict::new(py).unbind(),
    };
    graph.push_relation(Py::new(py, relation)?);
    Ok(())
}

/// The MODULE chain for a namespace: `App\Domain` puts MODULE `App\Domain`
/// inside MODULE `App`, and the leaf's id comes back.
///
/// The global namespace has no MODULE of its own — there is no name to give it —
/// so its files hang off the PROJECT node directly.
fn ensure_module_chain(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    namespace: &str,
    project_id: &str,
    modules: &mut IndexMap<String, String>,
) -> PyResult<String> {
    if namespace.is_empty() {
        return Ok(project_id.to_string());
    }
    let parts: Vec<&str> = namespace.split('\\').collect();
    let mut parent_id = project_id.to_string();

    for depth in 1..=parts.len() {
        let qname = parts[..depth].join("\\");
        let id = match modules.get(&qname) {
            Some(id) => id.clone(),
            None => {
                let id = node_id(project, &qname, NodeKind::Module.as_str());
                let node = Node {
                    id: id.clone(),
                    kind: NodeKind::Module,
                    qualified_name: qname.clone(),
                    name: parts[depth - 1].to_string(),
                    file_path: None,
                    span: None,
                    metadata: PyDict::new(py).unbind(),
                };
                graph.insert_node(id.clone(), Py::new(py, node)?);
                push_relation(py, graph, parent_id, id.clone(), RelationKind::Contains)?;
                modules.insert(qname, id.clone());
                id
            }
        };
        parent_id = id;
    }
    Ok(parent_id)
}

fn ensure_file(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    project_root: &Path,
    php_root: &Path,
    file: &Path,
    container_id: &str,
) -> PyResult<(String, String)> {
    // A FILE node's path is relative to the ORIGINAL project root, so the whole
    // monorepo shares one point of reference — and so the graph means the same
    // thing on another machine.
    let file_rel = file
        .strip_prefix(project_root)
        .or_else(|_| file.strip_prefix(php_root))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.to_string_lossy().into_owned());

    let file_id = node_id(project, &file_rel, NodeKind::File.as_str());
    if !graph.has_node(&file_id) {
        let node = Node {
            id: file_id.clone(),
            kind: NodeKind::File,
            qualified_name: file_rel.clone(),
            name: file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            file_path: Some(file_rel.clone()),
            span: None,
            metadata: PyDict::new(py).unbind(),
        };
        graph.insert_node(file_id.clone(), Py::new(py, node)?);
        push_relation(
            py,
            graph,
            container_id.to_string(),
            file_id.clone(),
            RelationKind::Contains,
        )?;
    }
    Ok((file_id, file_rel))
}

fn ensure_external_symbol(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    qname: &str,
    origin: &str,
) -> PyResult<String> {
    let sym_id = node_id(project, qname, NodeKind::ExternalSymbol.as_str());
    if !graph.has_node(&sym_id) {
        let metadata = PyDict::new(py);
        metadata.set_item("origin", origin)?;
        let node = Node {
            id: sym_id.clone(),
            kind: NodeKind::ExternalSymbol,
            qualified_name: qname.to_string(),
            name: qname.rsplit('\\').next().unwrap_or(qname).to_string(),
            file_path: None,
            span: None,
            metadata: metadata.unbind(),
        };
        graph.insert_node(sym_id.clone(), Py::new(py, node)?);
    }
    Ok(sym_id)
}

/// The namespace of every file that holds PHP at all.
///
/// This is a pre-pass rather than part of the file loop because the visitor
/// binds an internal `use` to the MODULE of its longest namespace prefix: every
/// namespace in the root needs a node before the first file is walked, or an
/// import lands on an EXTERNAL_SYMBOL depending on which file was read first.
/// It parses each file a second time instead of holding every tree in memory —
/// on a mid-sized application the trees outweigh the sources by an order of
/// magnitude.
///
/// A file that parses to `(program (text))` holds no PHP: that is a pure-HTML
/// template, which is legitimate and contributes nothing, so it is skipped
/// rather than reported as a failure.
fn file_namespaces(files: &[PathBuf]) -> PyResult<Vec<(PathBuf, String)>> {
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let Ok(source) = std::fs::read(file) else {
            continue;
        };
        let tree = super::parse_tree(&source)?;
        if !has_php_statements(tree.root_node()) {
            continue;
        }
        out.push((
            file.clone(),
            php_namespace(tree.root_node(), &source),
        ));
    }
    Ok(out)
}

/// Structure, imports and boundaries for one PHP project root.
fn build_root_structure(
    py: Python<'_>,
    graph: &mut Graph,
    project_root: &Path,
    php_root: &Path,
    files: &[PathBuf],
    extra_extractors: Option<&Py<PyAny>>,
) -> PyResult<BuiltRoot> {
    let project = project_name(php_root);
    let vendors = dependencies(php_root);
    let internal = internal_prefixes(php_root);

    let project_id = node_id(&project, &project, NodeKind::Project.as_str());
    if !graph.has_node(&project_id) {
        let node = Node {
            id: project_id.clone(),
            kind: NodeKind::Project,
            qualified_name: project.clone(),
            // Composer names a package `vendor/package`; the project is the
            // second half of that.
            name: project.rsplit('/').next().unwrap_or(&project).to_string(),
            file_path: None,
            span: None,
            metadata: PyDict::new(py).unbind(),
        };
        graph.insert_node(project_id.clone(), Py::new(py, node)?);
    }
    declare_dependencies(py, graph, &project, &project_id, &vendors)?;

    let namespaced = file_namespaces(files)?;
    let mut modules: IndexMap<String, String> = IndexMap::new();
    for (_file, namespace) in &namespaced {
        ensure_module_chain(py, graph, &project, namespace, &project_id, &mut modules)?;
    }

    let mut occurrences: Vec<(String, OccurrenceRef)> = Vec::new();
    let mut boundaries: Vec<FileBoundaries> = Vec::new();

    for (file, namespace) in &namespaced {
        let container_id = match modules.get(namespace) {
            Some(id) => id.clone(),
            None => project_id.clone(),
        };
        let (file_id, file_rel) =
            ensure_file(py, graph, &project, project_root, php_root, file, &container_id)?;

        let Ok(source) = std::fs::read(file) else {
            continue;
        };
        let tree = super::parse_tree(&source)?;

        let mut visitor = PhpVisitor::new(
            py,
            graph,
            &source,
            &project,
            &file_rel,
            namespace,
            &file_id,
            &internal,
            &vendors,
            &modules,
        );
        visitor.visit(tree.root_node())?;

        let abs_path = file.to_string_lossy().into_owned();
        for occurrence in visitor.into_occurrences() {
            occurrences.push((abs_path.clone(), occurrence));
        }

        // Read off the same tree but applied later: BOUNDARY nodes have to land
        // in the graph after the resolver's edges.
        let mut found = extract_boundaries(tree.root_node(), &source);
        found.extend(run_extractors(py, extra_extractors, &source, &file_rel)?);
        if !found.is_empty() {
            boundaries.push((file_rel, file_id, found));
        }
    }

    Ok((project, occurrences, boundaries))
}

/// A resolver's absolute path → a path relative to the project root.
///
/// Graph nodes store project-relative paths; without the conversion every
/// SpanIndex lookup misses.
fn relative_to(file_path: &str, project_root: &Path) -> String {
    let path = Path::new(file_path);
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    resolved
        .strip_prefix(project_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file_path.to_string())
}

/// The EXTERNAL_SYMBOL key for a target that could not be named.
///
/// The file path is part of the key deliberately: without it, sites sharing
/// coordinates across different files would collapse into one node (see the
/// README, "Differences from graphlens").
fn positional_key(project_root: &Path, file_path: &str, occurrence: &OccurrenceRef) -> String {
    let path = Path::new(file_path);
    let relative = path.strip_prefix(project_root).unwrap_or(path).to_string_lossy();
    format!(
        "{}@{}:{}:{}",
        occurrence.role, relative, occurrence.line, occurrence.col
    )
}

fn role_to_kind(role: &str) -> Option<RelationKind> {
    Some(match role {
        "call" => RelationKind::Calls,
        "base" => RelationKind::InheritsFrom,
        "annotation" => RelationKind::HasType,
        "read" | "write" => RelationKind::References,
        _ => return None,
    })
}

/// The language adapter for PHP projects.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct PhpAdapter {
    /// Extra boundary extractors, run in addition to the built-in ones.
    boundary_extractors: Option<Py<PyAny>>,
    resolver: Option<PhpResolver>,
}

#[gen_stub_pymethods]
#[pymethods]
impl PhpAdapter {
    /// Args:
    ///     resolve: False turns the resolution phase off — the graph stays
    ///         structural and `resolver_status` becomes `unavailable`.
    ///     boundary_extractors: extra boundary extractors, run in **addition**
    ///         to the built-in ones. Each is an object with
    ///         `extract(source: bytes, file_path: str) -> list[BoundaryRef]`.
    #[new]
    #[pyo3(signature = (*, resolve = true, boundary_extractors = None))]
    fn new(resolve: bool, boundary_extractors: Option<Py<PyAny>>) -> Self {
        Self {
            boundary_extractors,
            resolver: resolve.then(PhpResolver::empty),
        }
    }

    fn language(&self) -> &'static str {
        "php"
    }

    fn file_extensions(&self) -> HashSet<String> {
        PHP_EXTENSIONS.iter().map(|e| (*e).to_string()).collect()
    }

    fn can_handle(&self, project_root: PathBuf) -> bool {
        is_php(&project_root)
    }

    fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_php_files(&project_root)
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
                let roots = php_roots(&project_root);
                roots
                    .iter()
                    .map(|php_root| {
                        let collected = collect_php_files(php_root);
                        (
                            php_root.clone(),
                            filter_nested_root_files(collected, php_root, &roots),
                        )
                    })
                    .collect()
            }
        };

        let mut graph = Graph::empty(py);

        // Phase 1 — structure per root, no resolution. The SpanIndex built
        // below has to span the whole workspace, so a cross-root definition
        // already exists by the time the first occurrence looks for it.
        let mut built = Vec::with_capacity(root_files.len());
        for (php_root, root_sources) in &root_files {
            built.push(build_root_structure(
                py,
                &mut graph,
                &project_root,
                php_root,
                root_sources,
                self.boundary_extractors.as_ref(),
            )?);
        }

        // Phase 2 — ONE symbol table for the whole project_root. Indexing per
        // root would lose every cross-root reference and rebuild the table once
        // per root; a class in one package and its subclass in another are
        // exactly what a monorepo is for.
        let mut metrics = ResolverMetrics::default();
        let mut status = ResolverStatus::Unavailable;
        if let Some(resolver) = &mut self.resolver {
            let indexed: Vec<PathBuf> = root_files
                .iter()
                .flat_map(|(_root, sources)| sources.iter().cloned())
                .collect();
            resolver.prepare_rust(&project_root, &indexed)?;
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
            apply_boundaries(py, &mut graph, boundaries)?;
        }

        let status_value = status.as_str().into_pyobject(py)?;
        graph.set_metadata_item(py, RESOLVER_STATUS_KEY, status_value.as_any())?;
        graph.set_metadata_item(py, RESOLVER_METRICS_KEY, metrics.as_dict(py)?.as_any())?;

        if strict && status != ResolverStatus::Ok {
            return Err(AdapterError::new_err(format!(
                "PHP resolver status is '{}'; refusing to return a degraded graph in strict mode",
                status.as_str()
            )));
        }
        // One structural edge per pair: visitors emit one per occurrence, and
        // the node they point at is already deduped.
        graph.dedupe_structural_relations(py);

        Ok(graph)
    }

    fn __repr__(&self) -> String {
        format!(
            "PhpAdapter(resolver={})",
            if self.resolver.is_some() { "symbol table" } else { "off" }
        )
    }
}

fn resolve_pass(
    py: Python<'_>,
    resolver: &PhpResolver,
    graph: &mut Graph,
    span_index: &mut SpanIndex,
    project: &str,
    project_root: &Path,
    occurrences: &[(String, OccurrenceRef)],
) -> PyResult<ResolverMetrics> {
    let mut metrics = ResolverMetrics {
        queries: occurrences.len() as u64,
        ..Default::default()
    };
    if occurrences.is_empty() {
        return Ok(metrics);
    }

    let started = Instant::now();
    let refs: Vec<Option<ResolvedRef>> = occurrences
        .iter()
        .map(|(path, occurrence)| resolver.resolve_rust(path, occurrence.line, occurrence.col))
        .collect();
    metrics.seconds = started.elapsed().as_secs_f64();

    for ((path, occurrence), reference) in occurrences.iter().zip(refs.iter()) {
        let Some(reference) = reference else {
            metrics.unresolved += 1;
            continue;
        };
        metrics.resolved += 1;
        let Some(rel_kind) = role_to_kind(&occurrence.role) else {
            continue;
        };

        let mut target_id = None;
        if reference.origin == "internal"
            && let Some(file_path) = reference.file_path.as_deref()
        {
            let relative = relative_to(file_path, project_root);
            target_id = span_index.lookup_name(&relative, reference.line, reference.col);
        }

        match target_id {
            Some(_) => metrics.internal += 1,
            None => {
                metrics.external += 1;
                let fallback = if reference.full_name.is_empty() {
                    positional_key(project_root, path, occurrence)
                } else {
                    reference.full_name.clone()
                };
                target_id = Some(ensure_external_symbol(
                    py,
                    graph,
                    project,
                    &fallback,
                    &reference.origin,
                )?);
            }
        }

        let metadata = PyDict::new(py);
        metadata.set_item("span", occurrence.span)?;
        if occurrence.role == "read" || occurrence.role == "write" {
            metadata.set_item("access", &occurrence.role)?;
        }
        let relation = Relation {
            source_id: occurrence.enclosing_id.clone(),
            target_id: target_id.expect("the external symbol is created above"),
            kind: rel_kind,
            metadata: metadata.unbind(),
        };
        graph.push_relation(Py::new(py, relation)?);
    }
    Ok(metrics)
}

/// BOUNDARY nodes and EXPOSES / CONSUMES edges.
fn apply_boundaries(py: Python<'_>, graph: &mut Graph, files: &[FileBoundaries]) -> PyResult<()> {
    if files.is_empty() {
        return Ok(());
    }
    let mut enclosers: HashMap<String, Vec<(String, crate::span::Span)>> = HashMap::new();
    for node in graph.node_map().values() {
        let node = node.get();
        if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
            continue;
        }
        let (Some(file_path), Some(span)) = (node.file_path.as_ref(), node.span) else {
            continue;
        };
        enclosers
            .entry(file_path.clone())
            .or_default()
            .push((node.id.clone(), span));
    }

    for (file_rel, file_id, refs) in files {
        let candidates = enclosers.get(file_rel).map(Vec::as_slice).unwrap_or(&[]);
        for reference in refs {
            let enclosing_id =
                crate::python::innermost_enclosing_rust(candidates, reference.line, reference.col)
                    .unwrap_or_else(|| file_id.clone());
            crate::python::add_boundary_rust(py, graph, &enclosing_id, reference)?;
        }
    }
    Ok(())
}
