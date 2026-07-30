//! Orchestration of C and C++ analysis.
//!
//! Two `#[pyclass]`es over one implementation, each pinning a [`Dialect`]. A
//! mixed repository is analysed twice and the graphs merged; `headers::claims`
//! guarantees each file, including each ambiguous `.h`, is claimed by exactly
//! one of them, so the merge cannot collide.
//!
//! `analyze` runs FOUR passes rather than the family's usual three, and the
//! extra one is the point of the file:
//!
//! 1. **Scopes.** Every file's record and namespace names, collected before
//!    anything else, because `X::y` cannot be classified as a method or a
//!    namespaced function until the set of class names is known — and the class
//!    is usually declared in a different file from the definition.
//! 2. **Survey.** Every file read into [`FileFacts`], and every declaring site
//!    recorded in the [`DeclLedger`]. No node is written yet. A prototype in a
//!    header and its definition in a source file are ONE node, and whichever
//!    file happened to be read first would otherwise decide its span.
//! 3. **Emit.** Nodes, structural edges and occurrences, in path order.
//! 4. **Resolve, then boundaries.** In that order, so BOUNDARY nodes land after
//!    resolver edges.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::boundaries::BoundaryRef;
use crate::boundaries::run_extractors;
use crate::dependencies::declare_dependencies;
use crate::error::AdapterError;
use crate::graph::Graph;
use crate::ids::node_id;
use crate::metrics::{RESOLVER_METRICS_KEY, ResolverMetrics};
use crate::node::{Node, NodeKind};
use crate::python::apply_boundaries_rust;
use crate::relation::{Relation, RelationKind};
use crate::roots::filter_nested_root_files;
use crate::status::{RESOLVER_STATUS_KEY, ResolverStatus};

use super::boundary::extract_boundaries;
use super::deps::dependencies;
use super::detector::{
    C_EXTENSIONS, CPP_EXTENSIONS, c_roots, collect_c_files, collect_cpp_files, cpp_roots, is_c,
    is_cpp, project_name,
};
use super::headers::{DeclLedger, Linkage, claims, include_search_dirs};
use super::resolver::CFamilyIndex;
use super::visitor::{CFamilyEmitter, FileFacts, Owner, declared_scopes, read_file};
use super::{Dialect, parse_tree};

type FileBoundaries = (String, String, Vec<BoundaryRef>);

/// One file, carried between the four passes.
struct Analysed {
    file_rel: String,
    file_id: String,
    facts: FileFacts,
    boundaries: Vec<BoundaryRef>,
}

/// Reads a file and decides whether this dialect owns it.
///
/// Returns `None` for a file the other adapter claims, which is how an
/// ambiguous `.h` ends up in exactly one graph.
fn owned_source(dialect: Dialect, path: &Path) -> PyResult<Option<Vec<u8>>> {
    let Ok(source) = std::fs::read(path) else {
        return Ok(None);
    };
    Ok(claims(dialect, path, &source)?.then_some(source))
}

fn relative_to(file: &Path, project_root: &Path, lang_root: &Path) -> String {
    file.strip_prefix(project_root)
        .or_else(|_| file.strip_prefix(lang_root))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.to_string_lossy().into_owned())
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

/// The MODULE a file belongs to.
///
/// C has no module concept, so the directory is the only honest grouping — the
/// same choice the Go adapter makes. The root's own files land in a MODULE named
/// after the project rather than in one named by the empty string.
fn directory_module(file_rel: &str, project: &str) -> String {
    match Path::new(file_rel).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        _ => project.to_string(),
    }
}

fn ensure_module(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    qname: &str,
    project_id: &str,
    modules: &mut IndexMap<String, String>,
) -> PyResult<String> {
    if let Some(id) = modules.get(qname) {
        return Ok(id.clone());
    }
    let id = node_id(project, qname, NodeKind::Module.as_str());
    let node = Node {
        id: id.clone(),
        kind: NodeKind::Module,
        qualified_name: qname.to_string(),
        name: qname
            .rsplit(['/', ':'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(qname)
            .to_string(),
        file_path: None,
        span: None,
        metadata: PyDict::new(py).unbind(),
    };
    graph.insert_node(id.clone(), Py::new(py, node)?);
    push_relation(py, graph, project_id.to_string(), id.clone(), RelationKind::Contains)?;
    modules.insert(qname.to_string(), id.clone());
    Ok(id)
}

fn ensure_external_symbol(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    qname: &str,
    origin: &str,
) -> PyResult<String> {
    let id = node_id(project, qname, NodeKind::ExternalSymbol.as_str());
    if !graph.has_node(&id) {
        let metadata = PyDict::new(py);
        metadata.set_item("origin", origin)?;
        let node = Node {
            id: id.clone(),
            kind: NodeKind::ExternalSymbol,
            qualified_name: qname.to_string(),
            name: qname.rsplit("::").next().unwrap_or(qname).to_string(),
            file_path: None,
            span: None,
            metadata: metadata.unbind(),
        };
        graph.insert_node(id.clone(), Py::new(py, node)?);
    }
    Ok(id)
}

/// Use-site role → edge kind. Identical to the other adapters'.
fn role_to_kind(role: &str) -> Option<RelationKind> {
    Some(match role {
        "call" => RelationKind::Calls,
        "base" => RelationKind::InheritsFrom,
        "annotation" => RelationKind::HasType,
        "read" | "write" => RelationKind::References,
        _ => return None,
    })
}

/// Analyses one language root and returns its files, fully emitted.
#[allow(clippy::too_many_arguments)]
fn build_root(
    py: Python<'_>,
    graph: &mut Graph,
    dialect: Dialect,
    project_root: &Path,
    lang_root: &Path,
    files: &[PathBuf],
    extra_extractors: Option<&Py<PyAny>>,
) -> PyResult<(String, Vec<Analysed>)> {
    let project = project_name(lang_root);

    // Pass 1 — the scope names of every file, before anything is read in
    // earnest. `records` is what tells `Engine::ping` from `fixture::ping`.
    let mut records: HashSet<String> = HashSet::new();
    let mut sources: Vec<(PathBuf, String, Vec<u8>)> = Vec::new();
    for file in files {
        let Some(source) = owned_source(dialect, file)? else {
            continue;
        };
        let tree = parse_tree(&source, dialect)?;
        let scopes = declared_scopes(tree.root_node(), &source);
        // Only `records` is wanted: it is what classifies `X::y` as a method
        // rather than a namespaced function. `scopes.namespaces` is dropped
        // because MODULE is the directory here; see below.
        records.extend(scopes.records);
        let file_rel = relative_to(file, project_root, lang_root);
        sources.push((file.clone(), file_rel, source));
    }

    // Pass 2 — read every file, and record every declaring site before the
    // first node exists.
    let mut ledger = DeclLedger::new();
    let mut read: Vec<(String, FileFacts, Vec<u8>)> = Vec::with_capacity(sources.len());
    for (file, file_rel, source) in &sources {
        let tree = parse_tree(source, dialect)?;
        let facts = read_file(tree.root_node(), source, dialect, file_rel, &records);
        facts.survey(&mut ledger);
        let _ = file;
        read.push((file_rel.clone(), facts, source.clone()));
    }

    // The PROJECT node and its dependencies.
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
    declare_dependencies(py, graph, &project, &project_id, &dependencies(lang_root))?;

    // MODULE is the DIRECTORY, in both dialects. A C++ namespace groups
    // declarations, but it does not contain files — it spans them — and every
    // other adapter's MODULE is the thing a FILE nests under. Emitting both
    // shapes put `fixture` beside `include` and `src` in one graph with no way
    // to tell which kind a MODULE was. Namespaces are not lost: they are in
    // every qualified name the visitor builds, which is where a scope with no
    // file of its own belongs.
    let mut modules: IndexMap<String, String> = IndexMap::new();

    let known_files: HashSet<String> = read.iter().map(|(rel, _, _)| rel.clone()).collect();
    let search_dirs = include_search_dirs(lang_root);

    // FILE nodes, before the emitter runs: an include has to be able to point
    // at a header's FILE node whichever order the files are walked in.
    let mut file_ids: HashMap<String, String> = HashMap::new();
    for (file_rel, _, _) in &read {
        let file_id = node_id(&project, file_rel, NodeKind::File.as_str());
        if !graph.has_node(&file_id) {
            let node = Node {
                id: file_id.clone(),
                kind: NodeKind::File,
                qualified_name: file_rel.clone(),
                name: Path::new(file_rel)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                file_path: Some(file_rel.clone()),
                span: None,
                metadata: PyDict::new(py).unbind(),
            };
            graph.insert_node(file_id.clone(), Py::new(py, node)?);
            let module_qname = directory_module(file_rel, &project);
            let module_id =
                ensure_module(py, graph, &project, &module_qname, &project_id, &mut modules)?;
            push_relation(py, graph, module_id, file_id.clone(), RelationKind::Contains)?;
        }
        file_ids.insert(file_rel.clone(), file_id);
    }

    // Pass 3 — emit.
    let emitter = CFamilyEmitter::new(py, &project, &ledger, &search_dirs, &known_files);
    let mut analysed = Vec::with_capacity(read.len());
    for (file_rel, facts, source) in read {
        let file_id = file_ids[&file_rel].clone();
        emitter.emit(graph, &facts, &file_id)?;

        let tree = parse_tree(&source, dialect)?;
        let mut boundaries = extract_boundaries(tree.root_node(), &source, dialect);
        boundaries.extend(run_extractors(py, extra_extractors, &source, &file_rel)?);

        analysed.push(Analysed { file_rel, file_id, facts, boundaries });
    }

    Ok((project, analysed))
}

/// Resolves every occurrence against the sources that were parsed.
///
/// The other four adapters ask a backend "what is defined at this position?"
/// and map the answer back through a `SpanIndex`. There is no such backend
/// here, so the direction is reversed: the index is keyed by name, and
/// `#include` reachability decides which declaration of that name a file means.
fn resolve(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    analysed: &[Analysed],
) -> PyResult<ResolverMetrics> {
    let mut index = CFamilyIndex::new();

    for file in analysed {
        for decl in &file.facts.declarations {
            index.declare(
                &decl.name,
                &decl.qualified_name,
                decl.kind,
                &file.file_rel,
                decl.linkage == Linkage::Internal,
            );
        }
        for include in &file.facts.includes {
            index.include(&file.file_rel, &include.path);
        }
    }
    index.close_includes();

    let mut metrics = ResolverMetrics::default();
    for file in analysed {
        for occurrence in &file.facts.occurrences {
            let Some(kind) = role_to_kind(occurrence.role) else {
                continue;
            };
            metrics.queries += 1;

            let enclosing_id = match &occurrence.owner {
                Owner::File => file.file_id.clone(),
                Owner::Decl { qualified_name, kind } => {
                    node_id(project, qualified_name, kind.as_str())
                }
            };
            if !graph.has_node(&enclosing_id) {
                metrics.unresolved += 1;
                continue;
            }

            match index.lookup(&occurrence.name, &file.file_rel) {
                Some((qualified_name, target_kind, _ambiguous)) => {
                    let target_id = node_id(project, &qualified_name, target_kind.as_str());
                    if !graph.has_node(&target_id) {
                        metrics.unresolved += 1;
                        continue;
                    }
                    metrics.resolved += 1;
                    metrics.internal += 1;
                    push_relation(py, graph, enclosing_id, target_id, kind)?;
                }
                None => {
                    // Not declared in anything this translation unit can see: a
                    // libc function, a framework, or a macro-generated name. The
                    // edge is kept against an EXTERNAL_SYMBOL rather than
                    // dropped, and the key carries the site so two different
                    // unknowns at the same coordinates in different files stay
                    // apart.
                    let key = format!(
                        "{}@{}:{}:{}",
                        occurrence.name,
                        file.file_rel,
                        occurrence.span.start_line,
                        occurrence.span.start_col
                    );
                    let target_id =
                        ensure_external_symbol(py, graph, project, &key, "unknown")?;
                    metrics.resolved += 1;
                    metrics.external += 1;
                    push_relation(py, graph, enclosing_id, target_id, kind)?;
                }
            }
        }
    }
    Ok(metrics)
}

/// The body both adapters share.
fn analyze_family(
    py: Python<'_>,
    dialect: Dialect,
    resolve_symbols: bool,
    extractors: Option<&Py<PyAny>>,
    project_root: PathBuf,
    files: Option<Vec<PathBuf>>,
    strict: bool,
) -> PyResult<Graph> {
    let project_root = project_root.canonicalize().unwrap_or(project_root);

    let root_files: Vec<(PathBuf, Vec<PathBuf>)> = match files {
        Some(files) => vec![(project_root.clone(), files)],
        None => {
            let roots = match dialect {
                Dialect::C => c_roots(&project_root),
                Dialect::Cpp => cpp_roots(&project_root),
            };
            roots
                .iter()
                .map(|root| {
                    let collected = match dialect {
                        Dialect::C => collect_c_files(root),
                        Dialect::Cpp => collect_cpp_files(root),
                    };
                    (root.clone(), filter_nested_root_files(collected, root, &roots))
                })
                .collect()
        }
    };

    let mut graph = Graph::empty(py);

    let mut built: Vec<(String, Vec<Analysed>)> = Vec::with_capacity(root_files.len());
    for (lang_root, root_files) in &root_files {
        built.push(build_root(
            py,
            &mut graph,
            dialect,
            &project_root,
            lang_root,
            root_files,
            extractors,
        )?);
    }

    let mut metrics = ResolverMetrics::default();
    let mut status = ResolverStatus::Unavailable;
    if resolve_symbols {
        for (project, analysed) in &built {
            let pass = resolve(py, &mut graph, project, analysed)?;
            metrics.merge(&pass);
        }
        // A symbol table, never a type checker: see resolver.rs for the
        // reasoning and for what it cannot answer.
        status = ResolverStatus::Degraded;
    }

    for (_project, analysed) in &built {
        let boundaries: Vec<FileBoundaries> = analysed
            .iter()
            .filter(|file| !file.boundaries.is_empty())
            .map(|file| (file.file_rel.clone(), file.file_id.clone(), file.boundaries.clone()))
            .collect();
        apply_boundaries_rust(py, &mut graph, &boundaries)?;
    }

    let status_value = status.as_str().into_pyobject(py)?;
    graph.set_metadata_item(py, RESOLVER_STATUS_KEY, status_value.as_any())?;
    graph.set_metadata_item(py, RESOLVER_METRICS_KEY, metrics.as_dict(py)?.as_any())?;

    if strict && status != ResolverStatus::Ok {
        return Err(AdapterError::new_err(format!(
            "{} resolver status is '{}'; refusing to return a degraded graph in strict mode. \
             The C family has no compiler-backed resolver — see the docs for why.",
            dialect.as_str(),
            status.as_str()
        )));
    }

    graph.dedupe_structural_relations(py);
    Ok(graph)
}

/// The C adapter.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct CAdapter {
    resolve: bool,
    boundary_extractors: Option<Py<PyAny>>,
}

#[gen_stub_pymethods]
#[pymethods]
impl CAdapter {
    #[new]
    #[pyo3(signature = (*, resolve = true, boundary_extractors = None))]
    fn new(resolve: bool, boundary_extractors: Option<Py<PyAny>>) -> Self {
        Self { resolve, boundary_extractors }
    }

    fn language(&self) -> &'static str {
        "c"
    }

    fn file_extensions(&self) -> HashSet<String> {
        C_EXTENSIONS.iter().map(|e| (*e).to_string()).collect()
    }

    fn can_handle(&self, project_root: PathBuf) -> bool {
        is_c(&project_root)
    }

    fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_c_files(&project_root)
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
        analyze_family(
            py,
            Dialect::C,
            self.resolve,
            self.boundary_extractors.as_ref(),
            project_root,
            files,
            strict,
        )
    }

    fn __repr__(&self) -> String {
        format!(
            "CAdapter(resolver={})",
            if self.resolve { "symbol table" } else { "off" }
        )
    }
}

/// The C++ adapter.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct CppAdapter {
    resolve: bool,
    boundary_extractors: Option<Py<PyAny>>,
}

#[gen_stub_pymethods]
#[pymethods]
impl CppAdapter {
    #[new]
    #[pyo3(signature = (*, resolve = true, boundary_extractors = None))]
    fn new(resolve: bool, boundary_extractors: Option<Py<PyAny>>) -> Self {
        Self { resolve, boundary_extractors }
    }

    fn language(&self) -> &'static str {
        "cpp"
    }

    fn file_extensions(&self) -> HashSet<String> {
        CPP_EXTENSIONS.iter().map(|e| (*e).to_string()).collect()
    }

    fn can_handle(&self, project_root: PathBuf) -> bool {
        is_cpp(&project_root)
    }

    fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_cpp_files(&project_root)
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
        analyze_family(
            py,
            Dialect::Cpp,
            self.resolve,
            self.boundary_extractors.as_ref(),
            project_root,
            files,
            strict,
        )
    }

    fn __repr__(&self) -> String {
        format!(
            "CppAdapter(resolver={})",
            if self.resolve { "symbol table" } else { "off" }
        )
    }
}
