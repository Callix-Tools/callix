//! Orchestration of Rust crate analysis.
//!
//! A MODULE here is a Rust module (`crate::net::http`) rather than a
//! directory: the name is derived from the file's path inside `src`, and
//! nested `mod foo { ... }` blocks add their segments in the visitor.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use indexmap::IndexMap;
use crate::flow::Flow;
use crate::resolution::{self, Resolution};
use crate::py::prelude::*;
use crate::py::types::PyDict;
use crate::py::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::boundaries::BoundaryRef;
use crate::boundaries::run_extractors;
use crate::dependencies::{custom_declared, declare_dependencies};
use crate::error::AdapterError;
use crate::graph::Graph;
use crate::ids::node_id;
use crate::metrics::ResolverMetrics;
use crate::node::{Node, NodeKind};
use crate::occurrence::OccurrenceRef;
use crate::python::ResolvedRef;
use crate::relation::{Relation, RelationKind};
use crate::resolver_slot::ResolverSlot;
use crate::roots::{EXCLUDED_DIRS, collect_files, filter_nested_root_files};
use crate::span_index::SpanIndex;
use crate::status::ResolverStatus;

use super::boundary::extract_boundaries;
use super::deps::{crate_name, dependencies, is_rust, rust_roots};
use super::resolver::RustResolver;
use super::visitor::{RustExtractor, UseSite};

const RUST_EXTENSIONS: [&str; 1] = [".rs"];

type FileBoundaries = (String, String, Vec<BoundaryRef>);
/// `(file relative to the project root, line, col)` of a place a resolver may answer
/// with for a module -> the MODULE's id.
type ModuleSites = HashMap<(String, u32, u32), String>;
type BuiltCrate = (String, Vec<(String, OccurrenceRef)>, Vec<FileBoundaries>, ModuleSites);

/// Cargo writes `target/` at the workspace root, `cargo vendor` writes
/// `vendor/` beside it. Both are matched at the root only, so a crate keeps
/// its own `src/target/` or `src/vendor/` module.
const RUST_SKIP_AT_ROOT: [&str; 2] = ["target", "vendor"];

/// Rust sources under the root.
///
/// Without the root exclusions a built workspace contributes
/// `target/debug/deps/*.rs` and a vendored one contributes all of its
/// dependencies, both as first-party modules under invented paths.
pub fn collect_rust_files(root: &Path) -> Vec<PathBuf> {
    collect_files(root, &RUST_EXTENSIONS, &EXCLUDED_DIRS, &RUST_SKIP_AT_ROOT)
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

/// The absolute path of an internal `use` path: the root is translated into the
/// crate's namespace (`crate` -> the crate name, `self` -> the current module,
/// `super` -> the parent, the crate's own name -> itself). `None` for a relative path
/// or a `super` that climbs past the crate root.
fn absolute_use_path(import_path: &str, crate_name: &str, module_qname: &str) -> Option<String> {
    let segments: Vec<&str> = import_path
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let head = segments.first().copied()?;
    let own_parts: Vec<&str> = module_qname.split("::").collect();

    let (base, rest): (Vec<&str>, &[&str]) = match head {
        "crate" => (vec![crate_name], &segments[1..]),
        "self" => (own_parts.clone(), &segments[1..]),
        "super" => {
            let supers = segments.iter().take_while(|s| **s == "super").count();
            if supers >= own_parts.len() {
                return None;
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
        _ => return None,
    };

    let full: Vec<&str> = base.into_iter().chain(rest.iter().copied()).collect();
    if full.is_empty() {
        return None;
    }
    Some(full.join("::"))
}

/// The MODULE names an internal `use` path may lead to: the full path (the path is
/// the module) and its parent (the final segment is an imported item, not a module).
fn module_candidates(import_path: &str, crate_name: &str, module_qname: &str) -> Vec<String> {
    let Some(full) = absolute_use_path(import_path, crate_name, module_qname) else {
        return Vec::new();
    };
    let mut candidates = vec![full.clone()];
    if let Some((parent, _)) = full.rsplit_once("::") {
        candidates.push(parent.to_string());
    }
    candidates
}

/// The node a qualified name stands for: a type, an alias, a function or a constant of
/// the crate (tried in that fixed order), else a MODULE.
fn item_for_qname(
    graph: &Graph,
    project: &str,
    modules: &IndexMap<String, String>,
    qname: &str,
) -> Option<String> {
    for kind in [NodeKind::Class, NodeKind::TypeAlias, NodeKind::Function, NodeKind::Variable] {
        let id = node_id(project, qname, kind.as_str());
        if graph.has_node(&id) {
            return Some(id);
        }
    }
    modules.get(qname).cloned()
}

/// An edge that records where it was written. `name`: the binding is a lookup by path,
/// with no type checker behind it.
fn push_site_relation(
    py: Python<'_>,
    graph: &mut Graph,
    source_id: String,
    target_id: String,
    kind: RelationKind,
    span: crate::span::Span,
    access: Option<&str>,
) -> PyResult<()> {
    let metadata = PyDict::new(py);
    metadata.set_item("span", span)?;
    metadata.set_item(resolution::KEY, Resolution::Name.as_str())?;
    if let Some(access) = access {
        metadata.set_item("access", access)?;
    }
    graph.push_relation(Py::new(
        py,
        Relation { source_id, target_id, kind, metadata: metadata.unbind() },
    )?);
    Ok(())
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
#[allow(clippy::too_many_arguments)]
fn build_crate_structure(
    py: Python<'_>,
    graph: &mut Graph,
    project_root: &Path,
    crate_root: &Path,
    files: &[PathBuf],
    declared: Option<HashSet<String>>,
    extra_extractors: Option<&Py<PyAny>>,
) -> PyResult<BuiltCrate> {
    let project = crate_name(crate_root).unwrap_or_else(|| {
        crate_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let deps: HashSet<String> = declared.unwrap_or_else(|| dependencies(crate_root));

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
    let mut module_sites: ModuleSites = HashMap::new();
    let mut use_sites: Vec<(String, UseSite)> = Vec::new();

    let mut project_macros: std::collections::HashSet<String> = std::collections::HashSet::new();
    for file in files {
        if let Ok(source) = std::fs::read(file) {
            project_macros.extend(super::defined_macros(&source));
        }
    }
    for file in files {
        let module = module_qname(file, crate_root, &project);
        let module_id = ensure_module(py, graph, &project, &module, &project_id, &mut modules)?;
        let (file_id, file_rel) =
            ensure_file(py, graph, &project, project_root, crate_root, file, &module_id)?;
        // rust-analyzer answers a path segment that names a file module with the start
        // of that file.
        module_sites.insert((file_rel.clone(), 1, 1), module_id.clone());

        let Ok(source) = std::fs::read(file) else {
            continue;
        };
        let mut tree = super::parse_tree(&source)?;
        // The calls in a macro's arguments are read as the expressions they are.
        let mut source = source;
        if let Some(blanked) = super::blank_std_macros(&source, &tree, &project_macros) {
            source = blanked;
            tree = super::parse_tree(&source)?;
        }

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
        let (file_occurrences, file_imports, inline_modules, file_sites) = extractor.into_parts();
        // `mod foo { .. }` is a MODULE too; the resolver answers with the start of its name.
        for (inline_qname, name_span) in inline_modules {
            let inline_id =
                ensure_module(py, graph, &project, &inline_qname, &project_id, &mut modules)?;
            module_sites.insert((file_rel.clone(), name_span.start_line, name_span.start_col), inline_id);
        }

        let abs_path = file.to_string_lossy().into_owned();
        for occurrence in file_occurrences {
            occurrences.push((abs_path.clone(), occurrence));
        }
        internal_imports.extend(file_imports);
        for site in file_sites {
            use_sites.push((abs_path.clone(), site));
        }

        let mut found = extract_boundaries(tree.root_node(), &source);
        found.extend(run_extractors(py, extra_extractors, &source, &file_rel)?);
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

    // Names a `use` brings in and `mod x;`, bound by path once every file of the crate
    // has been read (`mod util;` in lib.rs is met before util.rs). What binds here is
    // never queried, so a resolver cannot answer the same site twice; the rest goes to
    // the resolver as `import` occurrences.
    for (abs_path, site) in use_sites {
        let span = site.occurrence.span;
        if site.mod_decl {
            let qname = format!("{}::{}", site.module_qname, site.path);
            if let Some(target) = modules.get(&qname) {
                push_site_relation(
                    py,
                    graph,
                    site.occurrence.enclosing_id.clone(),
                    target.clone(),
                    RelationKind::References,
                    span,
                    Some("read"),
                )?;
            }
            continue;
        }
        let qname = match site.origin {
            "internal" => absolute_use_path(&site.path, &project, &site.module_qname),
            // A 2018 uniform path, relative to the importing module (`use store::X`).
            "unknown" => Some(format!("{}::{}", site.module_qname, site.path)),
            _ => None,
        };
        let target = qname.and_then(|q| item_for_qname(graph, &project, &modules, &q));
        match target {
            Some(target_id) => push_site_relation(
                py,
                graph,
                site.occurrence.enclosing_id.clone(),
                target_id,
                RelationKind::ResolvesTo,
                span,
                None,
            )?,
            None => occurrences.push((abs_path, site.occurrence)),
        }
    }

    Ok((project, occurrences, boundaries, module_sites))
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
        "import" => RelationKind::ResolvesTo,
        _ => return None,
    })
}

/// The language adapter for Rust crates.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct RustAdapter {
    /// Extra boundary extractors, run in addition to the built-in ones.
    boundary_extractors: Option<Py<PyAny>>,
    dep_parsers: Option<Py<PyAny>>,
    resolver: ResolverSlot<RustResolver>,
}

#[gen_stub_pymethods]
#[pymethods]
impl RustAdapter {
    /// Args:
    ///     resolve: False turns the resolution phase off — the graph stays
    ///         structural and `resolver_status` becomes `unavailable`.
    ///     resolver: a custom symbol resolver, replacing the `rust-analyzer
    ///         scip` index. An object with `prepare(root, files)`,
    ///         `resolve_all(queries)` and `status()`.
    ///     dep_parsers: custom manifest parsers, replacing the built-in
    ///         `Cargo.toml` reader. Each is an object with `can_parse(root)` and
    ///         `parse(root)`.
    ///     boundary_extractors: extra boundary extractors, run in **addition**
    ///         to the built-in ones. Each is an object with
    ///         `extract(source: bytes, file_path: str) -> list[BoundaryRef]`.
    #[new]
    #[pyo3(signature = (
        *,
        resolve = true,
        resolver = None,
        dep_parsers = None,
        boundary_extractors = None,
    ))]
    pub fn new(
        resolve: bool,
        resolver: Option<Py<PyAny>>,
        dep_parsers: Option<Py<PyAny>>,
        boundary_extractors: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        Ok(Self {
            boundary_extractors,
            dep_parsers,
            resolver: ResolverSlot::new(resolve, resolver, || Ok(RustResolver::empty()))?,
        })
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

    pub fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_rust_files(&project_root)
    }

    /// Analyses the workspace and returns the graph.
    #[pyo3(signature = (project_root, files = None, *, strict = false))]
    pub fn analyze(
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
            built.push(build_crate_structure(
                py,
                &mut graph,
                &project_root,
                crate_root,
                files,
                custom_declared(py, self.dep_parsers.as_ref(), crate_root)?,
                self.boundary_extractors.as_ref(),
            )?);
        }

        // Phase 2 — ONE index for the whole workspace. Indexing each member
        // crate separately would both lose cross-crate calls and reload the
        // workspace once per member.
        let mut metrics = ResolverMetrics::default();
        let mut status = ResolverStatus::Unavailable;
        if !self.resolver.is_disabled() {
            let all_files: Vec<PathBuf> =
                crate_files.iter().flat_map(|(_, files)| files.clone()).collect();
            self.resolver.prepare(py, &project_root, &all_files)?;
            let mut span_index = SpanIndex::from_graph(py, &graph)?;
            let mut module_sites: ModuleSites = HashMap::new();
            for (_, _, _, sites) in &built {
                module_sites.extend(sites.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            for (project, occurrences, _boundaries, _sites) in &built {
                let pass = resolve_pass(
                    py,
                    &self.resolver,
                    &mut graph,
                    &mut span_index,
                    project,
                    &project_root,
                    occurrences,
                    &module_sites,
                )?;
                metrics.merge(&pass);
            }
            status = self.resolver.status(py)?;
        }

        // Phase 3 — boundaries; independent of resolution.
        for (_project, _occurrences, boundaries, _sites) in &built {
            apply_boundaries(py, &mut graph, boundaries)?;
        }

        graph.set_resolver_status(py, status)?;

        graph.set_resolver_metrics(py, &metrics)?;

        if strict && status != ResolverStatus::Ok {
            return Err(AdapterError::new_err(format!(
                "Rust resolver status is '{}'; refusing to return a degraded graph in strict mode",
                status.as_str()
            )));
        }
        // One structural edge per pair: visitors emit one per
        // occurrence, and the node they point at is already deduped.
        graph.dedupe_structural_relations(py);
        
        Ok(graph)
    }

    fn __repr__(&self) -> String {
        format!("RustAdapter(resolver={})", self.resolver.label("rust-analyzer scip"))
    }
}

/// Whether the node an answer was bound to is a parameter or a variable the enclosing
/// callable declared (a called local, `qualified_name` = `<enclosing>.<name>`): a value
/// read of one is not a reference to a definition of the project.
fn is_local_binding(graph: &Graph, bound: &str, enclosing: &str) -> bool {
    let nodes = graph.node_map();
    let (Some(bound), Some(enclosing)) = (nodes.get(bound), nodes.get(enclosing)) else {
        return false;
    };
    let (bound, enclosing) = (bound.get(), enclosing.get());
    match bound.kind {
        NodeKind::Parameter => true,
        NodeKind::Variable => bound
            .qualified_name
            .starts_with(&format!("{}.", enclosing.qualified_name)),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_pass(
    py: Python<'_>,
    resolver: &ResolverSlot<RustResolver>,
    graph: &mut Graph,
    span_index: &mut SpanIndex,
    project: &str,
    project_root: &Path,
    occurrences: &[(String, OccurrenceRef)],
    module_sites: &ModuleSites,
) -> PyResult<ResolverMetrics> {
    let mut metrics = ResolverMetrics {
        queries: occurrences.len() as u64,
        ..Default::default()
    };
    if occurrences.is_empty() {
        return Ok(metrics);
    }

    let started = Instant::now();
    let refs: Vec<Option<ResolvedRef>> = resolver.resolve_all(py, occurrences)?;
    metrics.seconds = started.elapsed().as_secs_f64();

    let mut flow = Flow::default();
    // The functions of the project by bare name, for a callee a macro's body wrote:
    // the resolver is asked about places in files, and the body's tokens are not one.
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    for (id, node) in graph.node_map().iter() {
        let node = node.get();
        if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
            by_name.entry(node.name.clone()).or_default().push(id.clone());
        }
    }
    for ((path, occurrence), reference) in occurrences.iter().zip(refs.iter()) {
        if let Some(name) = occurrence.detail.as_deref().and_then(|d| d.strip_prefix("name:")) {
            if let Some([only]) = by_name.get(name).map(Vec::as_slice) {
                metrics.resolved += 1;
                metrics.internal += 1;
                flow.call(path, occurrence, Some(only));
                let metadata = PyDict::new(py);
                metadata.set_item("span", occurrence.span)?;
                metadata.set_item(resolution::KEY, Resolution::Name.as_str())?;
                metadata.set_item("via", "macro body")?;
                graph.push_relation(Py::new(
                    py,
                    Relation {
                        source_id: occurrence.enclosing_id.clone(),
                        target_id: only.clone(),
                        kind: RelationKind::Calls,
                        metadata: metadata.unbind(),
                    },
                )?);
            }
            continue;
        }
        let Some(reference) = reference else {
            metrics.unresolved += 1;
            continue;
        };
        metrics.resolved += 1;
        let is_alias = occurrence.role == "alias";
        let rel_kind = match role_to_kind(&occurrence.role) {
            Some(kind) => kind,
            None if is_alias || occurrence.role == "callarg" => RelationKind::References,
            None => continue,
        };

        let mut target_id = None;
        if reference.origin == "internal"
            && let Some(file_path) = reference.file_path.as_deref()
        {
            let relative = relative_to(file_path, project_root);
            target_id = span_index.lookup_name(&relative, reference.line, reference.col);
            // A module has no name span in the index: `crate`, `super` and a file
            // module are answered with the start of the file, an inline `mod` with its name.
            if target_id.is_none() {
                target_id = module_sites
                    .get(&(relative, reference.line, reference.col))
                    .cloned();
            }
        }

        // A name a `use` brings in: an unbound answer with no name would only become a
        // site-keyed external symbol, which is noise. Dropped and counted unresolved.
        if occurrence.role == "import" && target_id.is_none() && reference.full_name.is_empty() {
            metrics.resolved -= 1;
            metrics.unresolved += 1;
            continue;
        }

        // `iter.map(f)`: the library calls what it is given, unless the callee is a
        // function of the project; the flow pass knows which.
        if occurrence.role == "callarg" {
            if let Some(held) = target_id.as_deref()
                && matches!(
                    graph.node_kind(held),
                    Some(NodeKind::Function | NodeKind::Method | NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute)
                )
                && let Some((line, col)) = occurrence.detail.as_deref().and_then(|d| {
                    let mut parts = d.strip_prefix("c:")?.split(':');
                    Some((parts.next()?.parse::<u32>().ok()?, parts.next()?.parse::<u32>().ok()?))
                })
            {
                flow.library_callback(&occurrence.enclosing_id, path, line, col, held, occurrence.span);
            }
            continue;
        }
        // What a call points at, and what a variable or a parameter holds, is
        // what the flow pass follows. An `alias` is not an edge of its own.
        if is_alias {
            if let Some(held) = target_id.as_deref() {
                match occurrence.detail.as_deref() {
                    Some("return") => flow.returned(&occurrence.enclosing_id, held),
                    Some("result") => flow.alias_result(&occurrence.enclosing_id, held),
                    _ => flow.alias(&occurrence.enclosing_id, held),
                }
            }
            continue;
        }
        if occurrence.role == "call" && occurrence.detail.as_deref() == Some("result") {
            if let Some(function) = target_id.as_deref() {
                flow.result_call(&occurrence.enclosing_id, function, occurrence.span, path);
            }
            continue;
        }
        // A name in expression or pattern position (`detail` `value`): an answer that
        // lands on a local or a parameter is no reference to a definition. No edge and
        // no EXTERNAL_SYMBOL.
        let is_value = occurrence.detail.as_deref() == Some(super::VALUE_DETAIL);
        if is_value
            && (reference.kind == "local"
                || target_id
                    .as_deref()
                    .is_some_and(|bound| is_local_binding(graph, bound, &occurrence.enclosing_id)))
        {
            continue;
        }
        if occurrence.role == "call" {
            flow.call(path, occurrence, target_id.as_deref());
        } else if is_value || occurrence.role == "import" {
            // A value read or an import is not an argument: nothing for the flow pass. It must not
            // reach the `detail.is_some()` branch below, which would drop it.
        } else if let Some(held) = target_id.as_deref() {
            flow.arg(path, occurrence, held);
        } else if occurrence.detail.is_some() {
            // A name handed to a call that is no node of the project (a local,
            // a standard-library function): the argument is only here to be
            // followed, and there is nothing to follow.
            continue;
        }

        // What the answer was bound to, before an unbound one becomes an external
        // symbol: that is what decides how the edge is described.
        let bound_kind = target_id.as_deref().and_then(|id| graph.node_kind(id));
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

        let resolution = Resolution::classify(rel_kind, bound_kind, &reference.origin);
        match resolution {
            Resolution::Value => metrics.to_value += 1,
            Resolution::Unlocated => metrics.unlocated += 1,
            _ => {}
        }
        let metadata = PyDict::new(py);
        metadata.set_item("span", occurrence.span)?;
        metadata.set_item(resolution::KEY, resolution.as_str())?;
        if occurrence.role == "read" || occurrence.role == "write" {
            metadata.set_item("access", &occurrence.role)?;
        }
        graph.note_site_file(
            &occurrence.enclosing_id,
            &relative_to(path, project_root),
            &metadata,
        )?;
        let relation = Relation {
            source_id: occurrence.enclosing_id.clone(),
            target_id: target_id.expect("the external symbol is created above"),
            kind: rel_kind,
            metadata: metadata.unbind(),
        };
        graph.push_relation(Py::new(py, relation)?);
    }
    flow.finish(py, graph)?;
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

#[cfg(test)]
mod tests {
    use super::absolute_use_path;

    #[test]
    fn crate_self_and_super_are_translated() {
        assert_eq!(absolute_use_path("crate::a::B", "demo", "demo::x").as_deref(), Some("demo::a::B"));
        assert_eq!(absolute_use_path("crate", "demo", "demo::x").as_deref(), Some("demo"));
        assert_eq!(absolute_use_path("self::B", "demo", "demo::x").as_deref(), Some("demo::x::B"));
        assert_eq!(absolute_use_path("super::f", "demo", "demo::x::y").as_deref(), Some("demo::x::f"));
        assert_eq!(absolute_use_path("super::f", "demo", "demo::x").as_deref(), Some("demo::f"));
    }

    #[test]
    fn super_past_the_root_and_relative_paths_have_no_absolute_form() {
        assert_eq!(absolute_use_path("super::super::f", "demo", "demo::x"), None);
        assert_eq!(absolute_use_path("super::f", "demo", "demo"), None);
        assert_eq!(absolute_use_path("store::X", "demo", "demo"), None);
        assert_eq!(absolute_use_path("", "demo", "demo"), None);
    }

    #[test]
    fn the_crates_own_name_is_the_root() {
        assert_eq!(absolute_use_path("demo::a", "demo", "demo::x").as_deref(), Some("demo::a"));
    }
}
