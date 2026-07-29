//! Orchestration of Rust crate analysis.
//!
//! A MODULE here is a Rust module (`crate::net::http`) rather than a
//! directory: the name is derived from the file's path inside `src`, and
//! nested `mod foo { ... }` blocks add their segments in the visitor.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::boundaries::BoundaryRef;
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
use super::deps::{crate_name, dependencies, is_rust, rust_roots};
use super::resolver::RustResolver;
use super::visitor::RustExtractor;

const RUST_EXTENSIONS: [&str; 1] = [".rs"];

type FileBoundaries = (String, String, Vec<BoundaryRef>);
type BuiltCrate = (String, Vec<(String, OccurrenceRef)>, Vec<FileBoundaries>);

pub fn collect_rust_files(root: &Path) -> Vec<PathBuf> {
    collect_files(root, &RUST_EXTENSIONS, &EXCLUDED_DIRS)
}

/// A module's qualified name from its file path within the crate.
///
/// `src/net/http.rs` → `crate_name::net::http`; trailing `lib`, `main`, and
/// `mod` are dropped — they name the enclosing module, not themselves.
fn module_qname(file: &Path, crate_root: &Path, crate_name: &str) -> String {
    let relative = file
        .strip_prefix(crate_root.join("src"))
        .or_else(|_| file.strip_prefix(crate_root));
    let Ok(relative) = relative else {
        return crate_name.to_string();
    };

    let mut parts: Vec<String> = relative
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.last().is_some_and(|last| matches!(last.as_str(), "lib" | "main" | "mod")) {
        parts.pop();
    }
    if parts.is_empty() {
        return crate_name.to_string();
    }
    format!("{crate_name}::{}", parts.join("::"))
}

/// The MODULE names an internal `use` path may lead to.
///
/// The path root is translated into the crate's namespace (`crate` → the
/// crate name, `self` → the current module, `super` → the parent), after
/// which the full path is offered (the path is the module) along with its
/// parent (the final segment is an imported item, not a module).
fn module_candidates(import_path: &str, crate_name: &str, module_qname: &str) -> Vec<String> {
    let segments: Vec<&str> = import_path
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let Some(head) = segments.first().copied() else {
        return Vec::new();
    };
    let own_parts: Vec<&str> = module_qname.split("::").collect();

    let (base, rest): (Vec<&str>, &[&str]) = match head {
        "crate" => (vec![crate_name], &segments[1..]),
        "self" => (own_parts.clone(), &segments[1..]),
        "super" => {
            let supers = segments.iter().take_while(|s| **s == "super").count();
            if supers >= own_parts.len() {
                return Vec::new();
            }
            (
                own_parts[..own_parts.len() - supers].to_vec(),
                &segments[supers..],
            )
        }
        _ if head.replace('-', "_") == crate_name.replace('-', "_") => {
            (vec![crate_name], &segments[1..])
        }
        // The classifier only marks the cases above as internal.
        _ => return Vec::new(),
    };

    let full: Vec<&str> = base.into_iter().chain(rest.iter().copied()).collect();
    if full.is_empty() {
        return Vec::new();
    }
    let mut candidates = vec![full.join("::")];
    if full.len() > 1 {
        candidates.push(full[..full.len() - 1].join("::"));
    }
    candidates
}

fn ensure_module(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    module_qname: &str,
    project_id: &str,
    modules: &mut IndexMap<String, String>,
) -> PyResult<String> {
    if let Some(id) = modules.get(module_qname) {
        return Ok(id.clone());
    }
    let id = node_id(project, module_qname, NodeKind::Module.as_str());
    let node = Node {
        id: id.clone(),
        kind: NodeKind::Module,
        qualified_name: module_qname.to_string(),
        name: module_qname.rsplit("::").next().unwrap_or(module_qname).to_string(),
        file_path: None,
        span: None,
        metadata: PyDict::new(py).unbind(),
    };
    graph.insert_node(id.clone(), Py::new(py, node)?);
    push_relation(py, graph, project_id.to_string(), id.clone(), RelationKind::Contains)?;
    modules.insert(module_qname.to_string(), id.clone());
    Ok(id)
}

fn ensure_file(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    project_root: &Path,
    crate_root: &Path,
    file: &Path,
    module_id: &str,
) -> PyResult<(String, String)> {
    let file_rel = file
        .strip_prefix(project_root)
        .or_else(|_| file.strip_prefix(crate_root))
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
        push_relation(py, graph, module_id.to_string(), file_id.clone(), RelationKind::Contains)?;
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
            name: qname.rsplit("::").next().unwrap_or(qname).to_string(),
            file_path: None,
            span: None,
            metadata: metadata.unbind(),
        };
        graph.insert_node(sym_id.clone(), Py::new(py, node)?);
    }
    Ok(sym_id)
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

/// Structure and imports for one crate.
fn build_crate_structure(
    py: Python<'_>,
    graph: &mut Graph,
    project_root: &Path,
    crate_root: &Path,
    files: &[PathBuf],
) -> PyResult<BuiltCrate> {
    let project = crate_name(crate_root).unwrap_or_else(|| {
        crate_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let deps: HashSet<String> = dependencies(crate_root);

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
    declare_dependencies(py, graph, &project, &project_id, &deps)?;

    let mut modules: IndexMap<String, String> = IndexMap::new();
    let mut occurrences: Vec<(String, OccurrenceRef)> = Vec::new();
    let mut boundaries: Vec<FileBoundaries> = Vec::new();
    let mut internal_imports: Vec<(String, String, String, String)> = Vec::new();

    for file in files {
        let module = module_qname(file, crate_root, &project);
        let module_id = ensure_module(py, graph, &project, &module, &project_id, &mut modules)?;
        let (file_id, file_rel) =
            ensure_file(py, graph, &project, project_root, crate_root, file, &module_id)?;

        let Ok(source) = std::fs::read(file) else {
            continue;
        };
        let tree = super::parse_tree(&source)?;

        let mut extractor = RustExtractor::new(
            py,
            graph,
            &source,
            &project,
            &module,
            &file_id,
            &file_rel,
            Some(&project),
            &deps,
        );
        extractor.extract(tree.root_node())?;
        let (file_occurrences, file_imports) = extractor.into_parts();

        let abs_path = file.to_string_lossy().into_owned();
        for occurrence in file_occurrences {
            occurrences.push((abs_path.clone(), occurrence));
        }
        internal_imports.extend(file_imports);

        let found = extract_boundaries(tree.root_node(), &source);
        if !found.is_empty() {
            boundaries.push((file_rel, file_id, found));
        }
    }

    // Internal imports are bound once every module of the crate exists.
    // Anything not found falls through to an EXTERNAL_SYMBOL so the
    // RESOLVES_TO edge is never lost.
    for (import_id, import_path, importer_qname, importer_file) in internal_imports {
        let target_id = module_candidates(&import_path, &project, &importer_qname)
            .into_iter()
            .find_map(|candidate| modules.get(&candidate).cloned());
        let target_id = match target_id {
            Some(id) => id,
            None => ensure_external_symbol(py, graph, &project, &import_path, "internal")?,
        };
        push_relation(py, graph, importer_file, target_id.clone(), RelationKind::Imports)?;
        push_relation(py, graph, import_id, target_id, RelationKind::ResolvesTo)?;
    }

    Ok((project, occurrences, boundaries))
}

/// A resolver's absolute path → a path relative to the project root.
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
/// The file path is part of the key deliberately: without it, calls on the
/// same line in different files would collapse into one node (see the README,
/// "Differences from graphlens").
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

/// The language adapter for Rust crates.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct RustAdapter {
    resolver: Option<RustResolver>,
}

#[gen_stub_pymethods]
#[pymethods]
impl RustAdapter {
    /// Args:
    ///     resolve: False turns the resolution phase off — the graph stays
    ///         structural and `resolver_status` becomes `unavailable`.
    #[new]
    #[pyo3(signature = (*, resolve = true))]
    fn new(resolve: bool) -> Self {
        Self {
            resolver: resolve.then(RustResolver::empty),
        }
    }

    fn language(&self) -> &'static str {
        "rust"
    }

    fn file_extensions(&self) -> HashSet<String> {
        RUST_EXTENSIONS.iter().map(|e| (*e).to_string()).collect()
    }

    fn can_handle(&self, project_root: PathBuf) -> bool {
        is_rust(&project_root)
    }

    fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_rust_files(&project_root)
    }

    /// Analyses the workspace and returns the graph.
    #[pyo3(signature = (project_root, files = None, *, strict = false))]
    fn analyze(
        &mut self,
        py: Python<'_>,
        project_root: PathBuf,
        files: Option<Vec<PathBuf>>,
        strict: bool,
    ) -> PyResult<Graph> {
        let project_root = project_root.canonicalize().unwrap_or(project_root);

        let crate_files: Vec<(PathBuf, Vec<PathBuf>)> = match files {
            Some(files) => vec![(project_root.clone(), files)],
            None => {
                let roots = rust_roots(&project_root);
                roots
                    .iter()
                    .map(|crate_root| {
                        let collected = collect_rust_files(crate_root);
                        (
                            crate_root.clone(),
                            filter_nested_root_files(collected, crate_root, &roots),
                        )
                    })
                    .collect()
            }
        };

        let mut graph = Graph::empty(py);

        // Phase 1 — structure per crate, no resolution: the SpanIndex below
        // has to cover the whole workspace, so cross-crate targets already
        // exist by the time the first occurrence is resolved.
        let mut built = Vec::with_capacity(crate_files.len());
        for (crate_root, files) in &crate_files {
            built.push(build_crate_structure(py, &mut graph, &project_root, crate_root, files)?);
        }

        // Phase 2 — ONE index for the whole workspace. Indexing each member
        // crate separately would both lose cross-crate calls and reload the
        // workspace once per member.
        let mut metrics = ResolverMetrics::default();
        let mut status = ResolverStatus::Unavailable;
        if let Some(resolver) = &mut self.resolver {
            resolver.prepare_rust(&project_root);
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

        // Phase 3 — boundaries; independent of resolution.
        for (_project, _occurrences, boundaries) in &built {
            apply_boundaries(py, &mut graph, boundaries)?;
        }

        let status_value = status.as_str().into_pyobject(py)?;
        graph.set_metadata_item(py, RESOLVER_STATUS_KEY, status_value.as_any())?;
        graph.set_metadata_item(py, RESOLVER_METRICS_KEY, metrics.as_dict(py)?.as_any())?;

        if strict && status != ResolverStatus::Ok {
            return Err(AdapterError::new_err(format!(
                "Rust resolver status is '{}'; refusing to return a degraded graph in strict mode",
                status.as_str()
            )));
        }
        Ok(graph)
    }

    fn __repr__(&self) -> String {
        format!(
            "RustAdapter(resolver={})",
            if self.resolver.is_some() { "rust-analyzer scip" } else { "off" }
        )
    }
}

fn resolve_pass(
    py: Python<'_>,
    resolver: &RustResolver,
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
        enclosers.entry(file_path.clone()).or_default().push((node.id.clone(), span));
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
