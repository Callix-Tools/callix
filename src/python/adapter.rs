//! The Python adapter's structural phase and the application of resolver
//! results.
//!
//! Analysis is split in two (see §7 of the graphlens architecture): structure
//! is built per sub-root, while resolution runs once for the whole call —
//! otherwise references between sub-roots could not resolve at all, and every
//! sub-root would pay for its own engine start-up.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use indexmap::IndexMap;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pyfunction, gen_stub_pymethods};

use crate::boundaries::BoundaryRef;
use crate::boundaries::run_extractors;
use crate::dependencies::declare_dependencies;
use crate::graph::Graph;
use crate::ids::{boundary_id, node_id};
use crate::error::AdapterError;
use crate::metrics::{RESOLVER_METRICS_KEY, ResolverMetrics};
use crate::node::{Node, NodeKind};
use crate::occurrence::{ImportClassifier, OccurrenceRef};
use crate::relation::{Relation, RelationKind};
use crate::roots::{EXCLUDED_DIRS, collect_files, filter_nested_root_files};
use crate::span::Span;
use crate::span_index::SpanIndex;
use crate::status::{RESOLVER_STATUS_KEY, ResolverStatus};

use super::boundary::extract_boundaries;
use super::deps::{parse_dependencies, stdlib_names};
use super::ty_embedded::EmbeddedTyResolver;
use super::detector::project_name;
use super::module_resolver::{qualified_name, source_root_for, source_roots};
use super::visitor::PythonVisitor;

const PY_EXTENSIONS: [&str; 2] = [".py", ".pyi"];

/// One file's boundaries: `(absolute path, FILE node id, ports)`.
type FileBoundaries = (String, String, Vec<BoundaryRef>);

/// The result of one root's structural phase.
type BuiltRoot = (String, Vec<(String, OccurrenceRef)>, Vec<FileBoundaries>);

/// A symbol the resolver bound to its definition. Coordinates are 1-based.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core", frozen, get_all, from_py_object)]
#[derive(Clone)]
pub struct ResolvedRef {
    pub full_name: String,
    pub file_path: Option<String>,
    pub line: u32,
    pub col: u32,
    pub kind: String,
    pub origin: String,
}

#[gen_stub_pymethods]
#[pymethods]
impl ResolvedRef {
    #[new]
    #[pyo3(signature = (full_name, file_path, line, col, kind, origin))]
    fn new(
        full_name: String,
        file_path: Option<String>,
        line: u32,
        col: u32,
        kind: String,
        origin: String,
    ) -> Self {
        Self { full_name, file_path, line, col, kind, origin }
    }

    fn __repr__(&self) -> String {
        format!(
            "ResolvedRef({:?} at {:?}:{}:{}, origin={})",
            self.full_name,
            self.file_path.as_deref().unwrap_or("-"),
            self.line,
            self.col,
            self.origin
        )
    }
}

/// Use-site role → edge kind.
fn role_to_kind(role: &str) -> Option<RelationKind> {
    Some(match role {
        "call" => RelationKind::Calls,
        "base" => RelationKind::InheritsFrom,
        "annotation" => RelationKind::HasType,
        "read" | "write" => RelationKind::References,
        _ => return None,
    })
}

fn add_node(py: Python<'_>, graph: &mut Graph, node: Node) -> PyResult<String> {
    let id = node.id.clone();
    graph.insert_node(id.clone(), Py::new(py, node)?);
    Ok(id)
}

fn add_relation(
    py: Python<'_>,
    graph: &mut Graph,
    source_id: String,
    target_id: String,
    kind: RelationKind,
    metadata: Option<Bound<'_, PyDict>>,
) -> PyResult<()> {
    let metadata = metadata.unwrap_or_else(|| PyDict::new(py));
    let relation = Relation { source_id, target_id, kind, metadata: metadata.unbind() };
    graph.push_relation(Py::new(py, relation)?);
    Ok(())
}

/// Creates the MODULE node chain for `a.b.c` and returns the leaf's id.
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
            add_node(py, graph, node)?;
            modules.insert(qname.clone(), id.clone());

            if let Some(parent_id) = &parent_id {
                add_relation(
                    py,
                    graph,
                    parent_id.clone(),
                    id,
                    RelationKind::Contains,
                    None,
                )?;
            }
        }
        parent_id = modules.get(&qname).cloned();
    }
    Ok(modules[module_qname].clone())
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
            name: qname.rsplit('.').next().unwrap_or(qname).to_string(),
            file_path: None,
            span: None,
            metadata: metadata.unbind(),
        };
        add_node(py, graph, node)?;
    }
    Ok(sym_id)
}

/// Every Python file under the root.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn collect_python_files(project_root: &str) -> Vec<String> {
    collect_files(Path::new(project_root), &PY_EXTENSIONS, &EXCLUDED_DIRS, &[])
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// Drops nested roots' files (a parent does not parse its subprojects).
#[gen_stub_pyfunction]
#[pyfunction]
pub fn filter_nested_files(
    files: Vec<String>,
    current_root: &str,
    project_roots: Vec<String>,
) -> Vec<String> {
    let files: Vec<PathBuf> = files.into_iter().map(PathBuf::from).collect();
    let roots: Vec<PathBuf> = project_roots.into_iter().map(PathBuf::from).collect();
    filter_nested_root_files(files, Path::new(current_root), &roots)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// Phase 1: structure and imports for one Python project root.
///
/// Returns `(project_name, [(absolute path, use-site)])`. Resolution and
/// boundary extraction happen later at project level, so that a single
/// resolver and a full `SpanIndex` serve every root at once.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (graph, project_root, py_root, files, stdlib, third_party))]
pub fn build_root_structure(
    py: Python<'_>,
    graph: &Bound<'_, Graph>,
    project_root: &str,
    py_root: &str,
    files: Vec<String>,
    stdlib: HashSet<String>,
    third_party: HashSet<String>,
) -> PyResult<BuiltRoot> {
    let mut graph = graph.borrow_mut();
    build_root_structure_inner(
        py, &mut graph, project_root, py_root, files, stdlib, third_party, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_root_structure_inner(
    py: Python<'_>,
    graph: &mut Graph,
    project_root: &str,
    py_root: &str,
    files: Vec<String>,
    stdlib: HashSet<String>,
    third_party: HashSet<String>,
    extra_extractors: Option<&Py<PyAny>>,
) -> PyResult<BuiltRoot> {
    let project_root = Path::new(project_root);
    let py_root = Path::new(py_root);
    let files: Vec<PathBuf> = files.into_iter().map(PathBuf::from).collect();

    let project = project_name(py_root);
    let roots = source_roots(py_root, &files);

    // Pre-pass: internal module names are derived from paths without parsing
    // sources — the classifier needs them before the first visit.
    let mut internal_tops: HashSet<String> = HashSet::new();
    for file in &files {
        let root = source_root_for(file, &roots).unwrap_or(&roots[0]);
        if let Some(qname) = qualified_name(file, root) {
            internal_tops.insert(qname.split('.').next().unwrap_or(&qname).to_string());
        }
    }

    let declared = third_party.clone();
    let classifier = ImportClassifier::from_sets(stdlib, third_party, internal_tops);

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
        add_node(py, graph, node)?;
    }
    declare_dependencies(py, graph, &project, &project_id, &declared)?;

    // Insertion order matters: PROJECT --CONTAINS--> top-level modules has to
    // come out in the same order a Python dict would give.
    let mut modules: IndexMap<String, String> = IndexMap::new();
    let mut all_occurrences: Vec<(String, OccurrenceRef)> = Vec::new();
    let mut all_boundaries: Vec<FileBoundaries> = Vec::new();

    for file in &files {
        let source_root = source_root_for(file, &roots).unwrap_or(&roots[0]);
        let Some(module_qname) = qualified_name(file, source_root) else {
            continue;
        };

        let leaf_module_id =
            ensure_module_chain(py, graph, &project, &module_qname, &mut modules)?;

        // A FILE node's path is relative to the ORIGINAL project_root, so the
        // whole monorepo shares one point of reference.
        let relative_path = file
            .strip_prefix(project_root)
            .or_else(|_| file.strip_prefix(py_root))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| file.to_string_lossy().into_owned());

        let file_rel = relative_path.clone();
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
            add_node(py, graph, node)?;
            add_relation(
                py,
                graph,
                leaf_module_id,
                file_id.clone(),
                RelationKind::Contains,
                None,
            )?;
        }

        let Ok(source) = std::fs::read(file) else {
            continue;
        };

        let abs_path = file.to_string_lossy().into_owned();
        let tree = super::parse_tree(&source)?;
        let mut visitor = PythonVisitor::new(
            py,
            graph,
            &source,
            &project,
            &file_rel,
            &module_qname,
            &file_id,
            &classifier,
        );
        visitor.visit(tree.root_node())?;
        for occurrence in visitor.into_occurrences() {
            all_occurrences.push((abs_path.clone(), occurrence));
        }

        // Boundaries are read off the same tree but applied later: BOUNDARY
        // nodes must land in the graph after the resolver's edges.
        let mut boundaries = extract_boundaries(tree.root_node(), &source);
        boundaries.extend(run_extractors(py, extra_extractors, &source, &file_rel)?);
        if !boundaries.is_empty() {
            all_boundaries.push((abs_path, file_id, boundaries));
        }
    }

    // PROJECT --CONTAINS--> top-level modules
    let top_level: Vec<String> = modules
        .iter()
        .filter(|(qname, _)| !qname.contains('.'))
        .map(|(_, id)| id.clone())
        .collect();
    for module_id in top_level {
        add_relation(
            py,
            graph,
            project_id.clone(),
            module_id,
            RelationKind::Contains,
            None,
        )?;
    }

    Ok((project, all_occurrences, all_boundaries))
}

/// Phase 2: turns resolver answers into edges.
///
/// Per use-site: an internal definition is looked up in the `SpanIndex`,
/// everything else falls through to an EXTERNAL_SYMBOL — so the edge is never
/// lost, even when the target lies outside the graph.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (graph, project_name, project_root, span_index, occurrences, refs))]
pub fn apply_resolutions(
    py: Python<'_>,
    graph: &Bound<'_, Graph>,
    project_name: &str,
    project_root: PathBuf,
    span_index: &Bound<'_, SpanIndex>,
    occurrences: Vec<(String, OccurrenceRef)>,
    refs: Vec<Option<ResolvedRef>>,
) -> PyResult<ResolverMetrics> {
    let mut graph = graph.borrow_mut();
    let mut span_index = span_index.borrow_mut();
    apply_resolutions_inner(
        py,
        &mut graph,
        project_name,
        &project_root,
        &mut span_index,
        &occurrences,
        &refs,
    )
}

/// A resolver's absolute path → a path relative to the project root.
///
/// Graph nodes store project-relative paths, so that a graph means the same
/// thing on another machine. Symlinks are resolved on the way, because a
/// resolver may answer with a canonicalized path where the walk found a link.
fn relative_to(file_path: &str, project_root: &Path) -> String {
    let path = Path::new(file_path);
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    resolved
        .strip_prefix(project_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file_path.to_string())
}

/// The key for an unresolved use-site.
///
/// The file path is mandatory: without it, sites sharing coordinates across
/// different files collapse into one node (graphlens keys on
/// `{role}@{line}:{col}` alone, and on superset that merged 98% of external
/// nodes). The path is taken relative to the project root so IDs stay
/// identical across machines.
fn positional_key(project_root: &Path, file_path: &str, occurrence: &OccurrenceRef) -> String {
    let path = Path::new(file_path);
    let relative = path
        .strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy();
    format!(
        "{}@{}:{}:{}",
        occurrence.role, relative, occurrence.line, occurrence.col
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_resolutions_inner(
    py: Python<'_>,
    graph: &mut Graph,
    project_name: &str,
    project_root: &Path,
    span_index: &mut SpanIndex,
    occurrences: &[(String, OccurrenceRef)],
    refs: &[Option<ResolvedRef>],
) -> PyResult<ResolverMetrics> {
    let mut metrics = ResolverMetrics {
        queries: occurrences.len() as u64,
        ..Default::default()
    };
    if occurrences.is_empty() {
        return Ok(metrics);
    }
    if occurrences.len() != refs.len() {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "occurrences and refs must have the same length",
        ));
    }

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
            // The resolver answers with an absolute path while nodes store
            // project-relative ones, so the lookup has to be converted — the
            // Go and Rust adapters do the same.
            let relative = relative_to(file_path, project_root);
            target_id = span_index.lookup_name(&relative, reference.line, reference.col);
        }

        match target_id {
            Some(_) => metrics.internal += 1,
            None => {
                metrics.external += 1;
                // Without a full_name the key is completed with file and
                // position, otherwise distinct unresolved sites would
                // collapse into a single node.
                let fallback = if reference.full_name.is_empty() {
                    positional_key(project_root, path, occurrence)
                } else {
                    reference.full_name.clone()
                };
                target_id = Some(ensure_external_symbol(
                    py,
                    graph,
                    project_name,
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
        add_relation(
            py,
            graph,
            occurrence.enclosing_id.clone(),
            target_id.expect("the external symbol is created above"),
            rel_kind,
            Some(metadata),
        )?;
    }
    Ok(metrics)
}

/// The deepest FUNCTION/METHOD containing the position.
///
/// On equal starts the last one in insertion order wins — the same way
/// iterating `graph.nodes.values()` behaves in the Python version.
fn innermost_enclosing(candidates: &[(String, Span)], line: u32, col: u32) -> Option<String> {
    let mut best: Option<(String, (u32, u32))> = None;
    for (id, span) in candidates {
        let start = (span.start_line, span.start_col);
        let end = (span.end_line, span.end_col);
        if start <= (line, col)
            && (line, col) <= end
            && best.as_ref().is_none_or(|(_, best_start)| start >= *best_start)
        {
            best = Some((id.clone(), start));
        }
    }
    best.map(|(id, _)| id)
}

/// Phase 3: BOUNDARY nodes and EXPOSES / CONSUMES edges.
///
/// A boundary's ID derives from `mechanism` + `key` alone, so a server in one
/// language and a client in another produce the same node and join up when
/// graphs are merged.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (graph, files))]
pub fn apply_boundaries(
    py: Python<'_>,
    graph: &Bound<'_, Graph>,
    files: Vec<FileBoundaries>,
) -> PyResult<()> {
    let mut graph = graph.borrow_mut();
    apply_boundaries_inner(py, &mut graph, &files)
}

pub(crate) fn apply_boundaries_rust(
    py: Python<'_>,
    graph: &mut Graph,
    files: &[FileBoundaries],
) -> PyResult<()> {
    apply_boundaries_inner(py, graph, files)
}

pub(crate) fn apply_resolutions_rust(
    py: Python<'_>,
    graph: &mut Graph,
    project_name: &str,
    project_root: &Path,
    span_index: &mut SpanIndex,
    occurrences: &[(String, OccurrenceRef)],
    refs: &[Option<ResolvedRef>],
) -> PyResult<ResolverMetrics> {
    apply_resolutions_inner(py, graph, project_name, project_root, span_index, occurrences, refs)
}

fn apply_boundaries_inner(
    py: Python<'_>,
    graph: &mut Graph,
    files: &[FileBoundaries],
) -> PyResult<()> {
    if files.is_empty() {
        return Ok(());
    }

    // Enclosing declarations, grouped per file in a single pass over the
    // graph.
    let mut enclosers: HashMap<String, Vec<(String, Span)>> = HashMap::new();
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

    for (file_path, file_id, refs) in files {
        let candidates = enclosers.get(file_path).map(Vec::as_slice).unwrap_or(&[]);
        for reference in refs {
            let enclosing_id = innermost_enclosing(candidates, reference.line, reference.col)
                .unwrap_or_else(|| file_id.clone());
            add_boundary(py, graph, &enclosing_id, reference)?;
        }
    }
    Ok(())
}

pub(crate) fn innermost_enclosing_rust(
    candidates: &[(String, Span)],
    line: u32,
    col: u32,
) -> Option<String> {
    innermost_enclosing(candidates, line, col)
}

pub(crate) fn add_boundary_rust(
    py: Python<'_>,
    graph: &mut Graph,
    enclosing_id: &str,
    reference: &BoundaryRef,
) -> PyResult<()> {
    add_boundary(py, graph, enclosing_id, reference)
}

fn add_boundary(
    py: Python<'_>,
    graph: &mut Graph,
    enclosing_id: &str,
    reference: &BoundaryRef,
) -> PyResult<()> {
    let boundary_id = boundary_id(&reference.mechanism, &reference.key);
    if !graph.has_node(&boundary_id) {
        let metadata = PyDict::new(py);
        metadata.set_item("mechanism", &reference.mechanism)?;
        metadata.set_item("key", &reference.key)?;
        let node = Node {
            id: boundary_id.clone(),
            kind: NodeKind::Boundary,
            qualified_name: format!("{}:{}", reference.mechanism, reference.key),
            name: reference.key.clone(),
            file_path: None,
            span: None,
            metadata: metadata.unbind(),
        };
        add_node(py, graph, node)?;
    }

    let kind = if reference.role == "server" {
        RelationKind::Exposes
    } else {
        RelationKind::Consumes
    };
    let metadata = PyDict::new(py);
    metadata.set_item("mechanism", &reference.mechanism)?;
    metadata.set_item("key", &reference.key)?;
    metadata.set_item("confidence", reference.confidence)?;
    metadata.set_item("role", &reference.role)?;
    metadata.set_item("line", reference.line)?;
    metadata.set_item("col", reference.col)?;
    for (key, value) in &reference.detail {
        metadata.set_item(key, value)?;
    }
    add_relation(
        py,
        graph,
        enclosing_id.to_string(),
        boundary_id,
        kind,
        Some(metadata),
    )
}

/// Where symbol definitions come from.
enum ResolverSlot {
    /// ty, linked into the module.
    Embedded(EmbeddedTyResolver),
    /// A Python object with `prepare` / `resolve_all` / `status`.
    Custom(Py<PyAny>),
    /// The resolution phase is off: the graph stays structural.
    Disabled,
}

/// Coerces an arbitrary resolver's answer into a native ResolvedRef.
fn coerce_ref(item: &Bound<'_, PyAny>) -> PyResult<Option<ResolvedRef>> {
    if item.is_none() {
        return Ok(None);
    }
    if let Ok(reference) = item.extract::<ResolvedRef>() {
        return Ok(Some(reference));
    }
    let attr_str = |name: &str| -> PyResult<String> {
        Ok(item
            .getattr(name)
            .ok()
            .filter(|v| !v.is_none())
            .map(|v| v.str())
            .transpose()?
            .map(|v| v.to_string())
            .unwrap_or_default())
    };
    let file_path = item
        .getattr("file_path")
        .ok()
        .filter(|v| !v.is_none())
        .map(|v| v.str())
        .transpose()?
        .map(|v| v.to_string());
    let attr_u32 = |name: &str| -> u32 {
        item.getattr(name)
            .ok()
            .and_then(|v| v.extract::<u32>().ok())
            .unwrap_or(0)
    };
    let origin = attr_str("origin")?;
    Ok(Some(ResolvedRef {
        full_name: attr_str("full_name")?,
        file_path,
        line: attr_u32("line"),
        col: attr_u32("col"),
        kind: attr_str("kind")?,
        origin: if origin.is_empty() { "unknown".to_string() } else { origin },
    }))
}

/// The language adapter for Python projects.
///
/// `analyze()` runs in three phases: structure per root, then a single
/// resolution pass for the whole call, then boundaries. The order is not
/// cosmetic — see the comments inside.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core", unsendable)]
pub struct PythonAdapter {
    dep_parsers: Option<Py<PyAny>>,
    resolver: ResolverSlot,
    /// Extra boundary extractors, run in addition to the built-in ones.
    boundary_extractors: Option<Py<PyAny>>,
}

impl PythonAdapter {
    /// Third-party package names: the built-in parsers, or custom ones.
    fn third_party(&self, py: Python<'_>, py_root: &Path) -> PyResult<HashSet<String>> {
        let Some(parsers) = &self.dep_parsers else {
            return Ok(parse_dependencies(&py_root.to_string_lossy()));
        };
        let root = py_root.to_string_lossy().into_owned();
        let path = py.import("pathlib")?.getattr("Path")?.call1((root,))?;
        let mut names = HashSet::new();
        for parser in parsers.bind(py).try_iter()? {
            let parser = parser?;
            if parser.call_method1("can_parse", (&path,))?.is_truthy()? {
                names.extend(
                    parser
                        .call_method1("parse", (&path,))?
                        .extract::<HashSet<String>>()?,
                );
            }
        }
        Ok(names)
    }

    /// Asks the resolver for definitions for every use-site.
    #[allow(clippy::too_many_arguments)]
    fn resolve(
        &mut self,
        py: Python<'_>,
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
        let refs: Vec<Option<ResolvedRef>> = match &self.resolver {
            ResolverSlot::Disabled => return Ok(ResolverMetrics::default()),
            ResolverSlot::Embedded(resolver) => occurrences
                .iter()
                .map(|(path, occurrence)| {
                    resolver.resolve_rust(path, occurrence.line, occurrence.col)
                })
                .collect(),
            ResolverSlot::Custom(resolver) => {
                let path_type = py.import("pathlib")?.getattr("Path")?;
                let queries = PyList::empty(py);
                for (path, occurrence) in occurrences {
                    let path = path_type.call1((path,))?;
                    queries.append((path, occurrence.line, occurrence.col))?;
                }
                let answers = resolver.bind(py).call_method1("resolve_all", (queries,))?;
                let mut refs = Vec::with_capacity(occurrences.len());
                for item in answers.try_iter()? {
                    refs.push(coerce_ref(&item?)?);
                }
                refs
            }
        };
        let elapsed = started.elapsed().as_secs_f64();

        let mut metrics =
            apply_resolutions_inner(
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
}

#[gen_stub_pymethods]
#[pymethods]
impl PythonAdapter {
    /// Args:
    ///     dep_parsers: custom manifest parsers (objects with `can_parse` and
    ///         `parse`). Defaults to the built-in ones: pyproject.toml,
    ///         requirements*.txt, setup.cfg.
    ///     resolver: a custom symbol resolver (an object with `resolve_all`).
    ///         Defaults to the embedded ty.
    ///     resolve: False turns the resolution phase off — the graph stays
    ///         structural and `resolver_status` in the metadata becomes
    ///         `unavailable`.
    ///     boundary_extractors: extra boundary extractors, run in **addition**
    ///         to the built-in ones. Each is an object with
    ///         `extract(source: bytes, file_path: str) -> list[BoundaryRef]`.
    #[new]
    #[pyo3(signature = (
        dep_parsers = None,
        resolver = None,
        *,
        resolve = true,
        boundary_extractors = None,
    ))]
    fn new(
        py: Python<'_>,
        dep_parsers: Option<Py<PyAny>>,
        resolver: Option<Py<PyAny>>,
        resolve: bool,
        boundary_extractors: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let resolver = match (resolve, resolver) {
            (false, _) => ResolverSlot::Disabled,
            (true, Some(custom)) => ResolverSlot::Custom(custom),
            (true, None) => ResolverSlot::Embedded(EmbeddedTyResolver::for_running_python(py)?),
        };
        Ok(Self { dep_parsers, resolver, boundary_extractors })
    }

    fn language(&self) -> &'static str {
        "python"
    }

    fn file_extensions(&self) -> HashSet<String> {
        PY_EXTENSIONS.iter().map(|e| (*e).to_string()).collect()
    }

    fn can_handle(&self, project_root: PathBuf) -> bool {
        super::detector::is_python_project(&project_root.to_string_lossy())
    }

    /// Every source under the root, excluding service directories.
    fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_files(&project_root, &PY_EXTENSIONS, &EXCLUDED_DIRS, &[])
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
        let project_root = project_root
            .canonicalize()
            .unwrap_or(project_root);
        let root_str = project_root.to_string_lossy().into_owned();

        let root_files: Vec<(PathBuf, Vec<String>)> = match files {
            Some(files) => vec![(
                project_root.clone(),
                files
                    .into_iter()
                    .map(|f| f.to_string_lossy().into_owned())
                    .collect(),
            )],
            None => {
                let roots = super::detector::python_roots(&project_root);
                roots
                    .iter()
                    .map(|py_root| {
                        let collected = collect_files(py_root, &PY_EXTENSIONS, &EXCLUDED_DIRS, &[]);
                        let kept = filter_nested_root_files(collected, py_root, &roots)
                            .into_iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect();
                        (py_root.clone(), kept)
                    })
                    .collect()
            }
        };

        let mut graph = Graph::empty(py);
        let stdlib = stdlib_names(py)?;

        // Phase 1 — structure per root, no resolution: the SpanIndex below
        // has to span the whole workspace, or cross-root definitions would
        // have nothing to bind to.
        let mut built = Vec::with_capacity(root_files.len());
        for (py_root, file_list) in &root_files {
            built.push(build_root_structure_inner(
                py,
                &mut graph,
                &root_str,
                &py_root.to_string_lossy(),
                file_list.clone(),
                stdlib.clone(),
                self.third_party(py, py_root)?,
                self.boundary_extractors.as_ref(),
            )?);
        }

        // Phase 2 — ONE resolver for the whole project_root. That is exactly
        // what lets cross-root references resolve without reindexing the
        // workspace once per root.
        let mut metrics = ResolverMetrics::default();
        let mut status = ResolverStatus::Unavailable;
        if !matches!(self.resolver, ResolverSlot::Disabled) {
            let all_files: Vec<String> =
                root_files.iter().flat_map(|(_, fs)| fs.clone()).collect();
            self.prepare_resolver(py, &root_str, all_files)?;

            let mut span_index = SpanIndex::from_graph(py, &graph)?;
            for (project, occurrences, _boundaries) in &built {
                let pass = self.resolve(
                    py,
                    &mut graph,
                    &mut span_index,
                    project,
                    &project_root,
                    occurrences,
                )?;
                metrics.merge(&pass);
            }
            status = self.resolver_status(py)?;
        }

        // Phase 3 — boundaries. After resolution, so BOUNDARY nodes land in
        // the graph last.
        for (_project, _occurrences, boundaries) in &built {
            apply_boundaries_inner(py, &mut graph, boundaries)?;
        }

        let status_value = status.as_str().into_pyobject(py)?;
        graph.set_metadata_item(py, RESOLVER_STATUS_KEY, status_value.as_any())?;
        graph.set_metadata_item(py, RESOLVER_METRICS_KEY, metrics.as_dict(py)?.as_any())?;

        if strict && status != ResolverStatus::Ok {
            return Err(AdapterError::new_err(format!(
                "Python resolver status is '{}'; refusing to return a degraded graph in strict mode",
                status.as_str()
            )));
        }
        Ok(graph)
    }

    fn __repr__(&self) -> String {
        let resolver = match &self.resolver {
            ResolverSlot::Embedded(_) => "ty",
            ResolverSlot::Custom(_) => "custom",
            ResolverSlot::Disabled => "off",
        };
        format!("PythonAdapter(resolver={resolver})")
    }
}

impl PythonAdapter {
    fn prepare_resolver(
        &mut self,
        py: Python<'_>,
        project_root: &str,
        files: Vec<String>,
    ) -> PyResult<()> {
        match &mut self.resolver {
            ResolverSlot::Embedded(resolver) => resolver.prepare_rust(project_root),
            ResolverSlot::Custom(resolver) => {
                let path_type = py.import("pathlib")?.getattr("Path")?;
                let root = path_type.call1((project_root,))?;
                let paths = PyList::empty(py);
                for file in files {
                    paths.append(path_type.call1((file,))?)?;
                }
                resolver.bind(py).call_method1("prepare", (root, paths))?;
                Ok(())
            }
            ResolverSlot::Disabled => Ok(()),
        }
    }

    fn resolver_status(&self, py: Python<'_>) -> PyResult<ResolverStatus> {
        match &self.resolver {
            ResolverSlot::Embedded(resolver) => Ok(resolver.status_rust()),
            ResolverSlot::Custom(resolver) => {
                let value = resolver.bind(py).call_method0("status")?;
                Ok(ResolverStatus::coerce(&value, ResolverStatus::Unavailable))
            }
            ResolverSlot::Disabled => Ok(ResolverStatus::Unavailable),
        }
    }
}
