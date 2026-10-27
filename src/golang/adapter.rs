//! Orchestration of Go project analysis.
//!
//! The node scheme differs from Python and TypeScript: a MODULE is a Go
//! package (a directory) rather than a dotted name, and its qualified_name
//! equals the import path, so internal imports are bound by direct lookup.

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
use crate::span::Span;
use crate::span_index::SpanIndex;
use crate::status::ResolverStatus;

use super::boundary::extract_boundaries;
use super::deps::{go_roots, is_go, module_path, required_modules};
use super::resolver::GoResolver;
use super::visitor::{GoExtractor, ImportSite};

const GO_EXTENSIONS: [&str; 1] = [".go"];

type FileBoundaries = (String, String, Vec<BoundaryRef>);
/// `(file, line, col)` of the position go/types answers for a package name ->
/// `(id of the node the import binds to, origin of the import)`.
type ImportSites = HashMap<(String, u32, u32), (String, String)>;
type BuiltRoot = (String, Vec<(String, OccurrenceRef)>, Vec<FileBoundaries>, ImportSites);

/// `testdata` is invisible to the Go toolchain at every depth, and its
/// contents are frequently not even valid Go — they are fixtures for a parser
/// test, not code the module builds.
const GO_SKIP_ANYWHERE: [&str; 1] = ["testdata"];

/// `go mod vendor` writes the third-party tree at the module root.
const GO_SKIP_AT_ROOT: [&str; 1] = ["vendor"];

/// Go sources under the root.
///
/// graphlens collected `vendor/` and `testdata/` as first-party — its Go
/// adapter excluded them from root discovery only, then inherited the base
/// class's file collection. That reported a vendored dependency as project
/// code under a fabricated import path, so callix diverges here.
pub fn collect_go_files(root: &Path) -> Vec<PathBuf> {
    let excluded: Vec<&str> = EXCLUDED_DIRS
        .iter()
        .chain(GO_SKIP_ANYWHERE.iter())
        .copied()
        .collect();
    collect_files(root, &GO_EXTENSIONS, &excluded, &GO_SKIP_AT_ROOT)
}

/// A package's qualified name: the module path plus the file's directory
/// within it.
fn package_qname(file: &Path, go_root: &Path, module_path: &str) -> String {
    let Some(parent) = file.parent() else {
        return module_path.to_string();
    };
    match parent.strip_prefix(go_root) {
        // A file outside the module root (passed explicitly) counts as the
        // root package.
        Err(_) => module_path.to_string(),
        Ok(rel) if rel.as_os_str().is_empty() => module_path.to_string(),
        Ok(rel) => format!("{module_path}/{}", rel.to_string_lossy()),
    }
}

fn ensure_package(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    pkg_qname: &str,
    project_id: &str,
    packages: &mut IndexMap<String, String>,
    file_rel: Option<String>,
) -> PyResult<String> {
    if let Some(id) = packages.get(pkg_qname) {
        return Ok(id.clone());
    }
    let id = node_id(project, pkg_qname, NodeKind::Module.as_str());
    let node = Node {
        id: id.clone(),
        kind: NodeKind::Module,
        qualified_name: pkg_qname.to_string(),
        name: pkg_qname.rsplit('/').next().unwrap_or(pkg_qname).to_string(),
        // The first non-test file of the package (chosen from the paths before the
        // walk). `span` stays None: a package spans several files.
        file_path: file_rel,
        span: None,
        metadata: PyDict::new(py).unbind(),
    };
    graph.insert_node(id.clone(), Py::new(py, node)?);
    push_relation(py, graph, project_id.to_string(), id.clone(), RelationKind::Contains)?;
    packages.insert(pkg_qname.to_string(), id.clone());
    Ok(id)
}

/// A file's path relative to the project root (else the Go root).
fn file_rel_of(project_root: &Path, go_root: &Path, file: &Path) -> String {
    file.strip_prefix(project_root)
        .or_else(|_| file.strip_prefix(go_root))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.to_string_lossy().into_owned())
}

fn is_test_file(file: &Path) -> bool {
    file.file_name().is_some_and(|n| n.to_string_lossy().ends_with("_test.go"))
}

/// The file each package is placed at: its first file in walk order (which is
/// sorted), preferring the first that is not a `_test.go`, so that an external
/// test package does not win. Decided from the paths alone.
fn placement_files(go_root: &Path, files: &[PathBuf], module: &str) -> HashMap<String, PathBuf> {
    let mut placed: HashMap<String, PathBuf> = HashMap::new();
    for file in files {
        let qname = package_qname(file, go_root, module);
        let replace = match placed.get(&qname) {
            None => true,
            Some(current) => is_test_file(current) && !is_test_file(file),
        };
        if replace {
            placed.insert(qname, file.clone());
        }
    }
    placed
}

/// The span of the identifier in a file's `package` clause.
fn package_name_span(root: tree_sitter::Node<'_>) -> Option<Span> {
    let clause = crate::ts::children(root).into_iter().find(|n| n.kind() == "package_clause")?;
    crate::ts::children(clause)
        .into_iter()
        .find(|n| n.kind() == "package_identifier")
        .map(crate::ts::span)
}

fn ensure_file(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    project_root: &Path,
    go_root: &Path,
    file: &Path,
    module_id: &str,
) -> PyResult<(String, String)> {
    let file_rel = file_rel_of(project_root, go_root, file);

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
            module_id.to_string(),
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
            name: qname.rsplit('.').next().unwrap_or(qname).to_string(),
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

/// A structural edge that records where it was observed.
fn push_relation_with_span(
    py: Python<'_>,
    graph: &mut Graph,
    source_id: String,
    target_id: String,
    kind: RelationKind,
    span: Span,
) -> PyResult<()> {
    let metadata = PyDict::new(py);
    metadata.set_item("span", span)?;
    let relation = Relation { source_id, target_id, kind, metadata: metadata.unbind() };
    graph.push_relation(Py::new(py, relation)?);
    Ok(())
}

/// Structure and imports for one Go module root.
///
/// `declared` overrides the module paths read from `go.mod`; `None` means no
/// custom manifest parsers were supplied.
#[allow(clippy::too_many_arguments)]
fn build_root_structure(
    py: Python<'_>,
    graph: &mut Graph,
    project_root: &Path,
    go_root: &Path,
    files: &[PathBuf],
    declared: Option<HashSet<String>>,
    extra_extractors: Option<&Py<PyAny>>,
) -> PyResult<BuiltRoot> {
    let module = module_path(go_root).unwrap_or_else(|| {
        go_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let project = module
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(&module)
        .to_string();
    let required = declared.unwrap_or_else(|| required_modules(go_root));

    let project_id = node_id(&project, &module, NodeKind::Project.as_str());
    if !graph.has_node(&project_id) {
        let node = Node {
            id: project_id.clone(),
            kind: NodeKind::Project,
            qualified_name: module.clone(),
            name: project.clone(),
            file_path: None,
            span: None,
            metadata: PyDict::new(py).unbind(),
        };
        graph.insert_node(project_id.clone(), Py::new(py, node)?);
    }
    declare_dependencies(py, graph, &project, &project_id, &required)?;

    let mut packages: IndexMap<String, String> = IndexMap::new();
    let mut occurrences: Vec<(String, OccurrenceRef)> = Vec::new();
    let mut boundaries: Vec<FileBoundaries> = Vec::new();
    let mut internal_imports: Vec<(String, String, String, Span)> = Vec::new();
    let mut pending_sites: Vec<(String, ImportSite)> = Vec::new();
    let placed_files = placement_files(go_root, files, &module);

    for file in files {
        let pkg_qname = package_qname(file, go_root, &module);
        let placed = placed_files.get(&pkg_qname);
        let placed_rel = placed.map(|p| file_rel_of(project_root, go_root, p));
        let module_id = ensure_package(
            py, graph, &project, &pkg_qname, &project_id, &mut packages, placed_rel,
        )?;
        let (file_id, file_rel) =
            ensure_file(py, graph, &project, project_root, go_root, file, &module_id)?;

        let Ok(source) = std::fs::read(file) else {
            continue;
        };
        let tree = super::parse_tree(&source)?;

        let mut extractor = GoExtractor::new(
            py,
            graph,
            &source,
            &project,
            &pkg_qname,
            &file_id,
            &file_rel,
            Some(&module),
            &required,
        );
        extractor.extract(tree.root_node())?;
        let (file_occurrences, file_imports, file_sites) = extractor.into_parts();

        // The package's name is placed at the package clause of its placement file.
        if placed.is_some_and(|p| p == file)
            && let Some(span) = package_name_span(tree.root_node())
            && let Some(node) = graph.node_map().get(&module_id)
        {
            node.get().metadata.bind(py).set_item("name_span", span)?;
        }

        let abs_path = file.to_string_lossy().into_owned();
        for occurrence in file_occurrences {
            occurrences.push((abs_path.clone(), occurrence));
        }
        internal_imports.extend(file_imports);
        pending_sites.extend(file_sites.into_iter().map(|site| (file_rel.clone(), site)));

        let mut found = extract_boundaries(tree.root_node(), &source);
        found.extend(run_extractors(py, extra_extractors, &source, &file_rel)?);
        if !found.is_empty() {
            boundaries.push((file_rel, file_id, found));
        }
    }

    // Internal imports are bound once every package exists: a Go import path
    // equals the package's qualified name, so a direct lookup suffices.
    // Anything not found falls through to an EXTERNAL_SYMBOL so the edge is
    // never lost.
    for (import_id, import_path, importer_file, path_span) in internal_imports {
        let target_id = match packages.get(&import_path) {
            Some(id) => id.clone(),
            None => ensure_external_symbol(py, graph, &project, &import_path, "internal")?,
        };
        push_relation(py, graph, importer_file, target_id.clone(), RelationKind::Imports)?;
        push_relation_with_span(
            py,
            graph,
            import_id,
            target_id,
            RelationKind::ResolvesTo,
            path_span,
        )?;
    }

    // Where each package name is declared (an alias, else the path literal), and
    // the node it binds to: the same node the import edges above point at.
    let mut import_sites = ImportSites::new();
    for (file_rel, site) in pending_sites {
        let target_id = if site.origin == "internal" {
            match packages.get(&site.path) {
                Some(id) => id.clone(),
                None => ensure_external_symbol(py, graph, &project, &site.path, "internal")?,
            }
        } else {
            node_id(&project, &site.path, NodeKind::ExternalSymbol.as_str())
        };
        import_sites.insert((file_rel, site.line, site.col), (target_id, site.origin.to_string()));
    }

    // A bare identifier can only denote a package-level object of its own package
    // (dot imports are out of scope), a predeclared name or a local. Keep only the
    // `name:` occurrences whose name is a VARIABLE or FUNCTION node of the file's
    // package, and remember that node's id: resolve_pass keeps the occurrence only
    // when go/types binds it there. The rest (locals, `nil`, `len`, ...) never
    // reach the resolver.
    occurrences.retain_mut(|(path, occurrence)| {
        let Some(name) = occurrence
            .detail
            .as_deref()
            .and_then(|detail| detail.strip_prefix("name:"))
            .map(str::to_string)
        else {
            return true;
        };
        let qname = format!("{}.{name}", package_qname(Path::new(path.as_str()), go_root, &module));
        for kind in [NodeKind::Variable, NodeKind::Function] {
            let id = node_id(&project, &qname, kind.as_str());
            if graph.has_node(&id) {
                occurrence.detail = Some(format!("name:{id}"));
                return true;
            }
        }
        false
    });

    Ok((project, occurrences, boundaries, import_sites))
}

/// A resolver's absolute path → a path relative to the project root.
///
/// The resolver hands back absolute paths while graph nodes store relative
/// ones; without the conversion, SpanIndex lookups miss.
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

/// The language adapter for Go projects.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core", unsendable)]
pub struct GoAdapter {
    /// Extra boundary extractors, run in addition to the built-in ones.
    boundary_extractors: Option<Py<PyAny>>,
    dep_parsers: Option<Py<PyAny>>,
    resolver: ResolverSlot<GoResolver>,
}

#[gen_stub_pymethods]
#[pymethods]
impl GoAdapter {
    /// Args:
    ///     resolve: False turns the resolution phase off — the graph stays
    ///         structural and `resolver_status` becomes `unavailable`.
    ///     resolver: a custom symbol resolver, replacing `go/packages`. An
    ///         object with `prepare(root, files)`, `resolve_all(queries)` and
    ///         `status()`.
    ///     dep_parsers: custom manifest parsers, replacing the built-in `go.mod`
    ///         reader. Each is an object with `can_parse(root)` and
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
            resolver: ResolverSlot::new(resolve, resolver, || Ok(GoResolver::empty()))?,
        })
    }

    fn language(&self) -> &'static str {
        "go"
    }

    fn file_extensions(&self) -> HashSet<String> {
        GO_EXTENSIONS.iter().map(|e| (*e).to_string()).collect()
    }

    fn can_handle(&self, project_root: PathBuf) -> bool {
        is_go(&project_root)
    }

    pub fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_go_files(&project_root)
    }

    /// Analyses the project and returns the graph.
    #[pyo3(signature = (project_root, files = None, *, strict = false))]
    pub fn analyze(
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
                let roots = go_roots(&project_root);
                roots
                    .iter()
                    .map(|go_root| {
                        let collected = collect_go_files(go_root);
                        (go_root.clone(), filter_nested_root_files(collected, go_root, &roots))
                    })
                    .collect()
            }
        };

        let mut graph = Graph::empty(py);

        // Phase 1 — structure per module, no resolution.
        let mut built = Vec::with_capacity(root_files.len());
        for (go_root, module_files) in &root_files {
            built.push(build_root_structure(
                py,
                &mut graph,
                &project_root,
                go_root,
                module_files,
                custom_declared(py, self.dep_parsers.as_ref(), go_root)?,
                self.boundary_extractors.as_ref(),
            )?);
        }

        // Phase 2 — one resolver for the whole project_root, so cross-module
        // calls resolve and the workspace is not reloaded per module.
        let mut metrics = ResolverMetrics::default();
        let mut status = ResolverStatus::Unavailable;
        if !self.resolver.is_disabled() {
            let all_files: Vec<PathBuf> =
                root_files.iter().flat_map(|(_, files)| files.clone()).collect();
            self.resolver.prepare(py, &project_root, &all_files)?;
            let mut span_index = SpanIndex::from_graph(py, &graph)?;
            for (project, occurrences, _boundaries, import_sites) in &built {
                let pass = resolve_pass(
                    py,
                    &self.resolver,
                    &mut graph,
                    &mut span_index,
                    project,
                    &project_root,
                    occurrences,
                    import_sites,
                )?;
                metrics.merge(&pass);
            }
            status = self.resolver.status(py)?;
        }

        // Phase 3 — boundaries, after resolution.
        for (_project, _occurrences, boundaries, _sites) in &built {
            apply_boundaries(py, &mut graph, boundaries)?;
        }

        graph.set_resolver_status(py, status)?;

        graph.set_resolver_metrics(py, &metrics)?;

        if strict && status != ResolverStatus::Ok {
            return Err(AdapterError::new_err(format!(
                "Go resolver status is '{}'; refusing to return a degraded graph in strict mode",
                status.as_str()
            )));
        }
        // One structural edge per pair: visitors emit one per
        // occurrence, and the node they point at is already deduped.
        graph.dedupe_structural_relations(py);
        
        Ok(graph)
    }

    fn __repr__(&self) -> String {
        format!("GoAdapter(resolver={})", self.resolver.label("go/packages"))
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_pass(
    py: Python<'_>,
    resolver: &ResolverSlot<GoResolver>,
    graph: &mut Graph,
    span_index: &mut SpanIndex,
    project: &str,
    project_root: &Path,
    occurrences: &[(String, OccurrenceRef)],
    import_sites: &ImportSites,
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
    for ((path, occurrence), reference) in occurrences.iter().zip(refs.iter()) {
        let Some(reference) = reference else {
            metrics.unresolved += 1;
            continue;
        };
        metrics.resolved += 1;
        // A package qualifier (`util` in `util.Double`). go/types answers a package
        // name with the position of its import (the alias, else the path literal);
        // anything else - a local that shadows the package, a cgo oddity - is not
        // a use of the package and gets no edge. The origin comes from the import,
        // not from the answer, which always lies in the importing file.
        if occurrence.detail.as_deref() == Some("package") {
            let Some(file_path) = reference.file_path.as_deref() else {
                continue;
            };
            let key = (relative_to(file_path, project_root), reference.line, reference.col);
            let Some((target, site_origin)) = import_sites.get(&key) else {
                continue;
            };
            let bound = match graph.node_kind(target) {
                Some(NodeKind::Module) => Some(NodeKind::Module),
                _ => None,
            };
            let resolution = Resolution::classify(RelationKind::References, bound, site_origin);
            if bound.is_some() {
                metrics.internal += 1;
            } else {
                metrics.external += 1;
            }
            if matches!(resolution, Resolution::Unlocated) {
                metrics.unlocated += 1;
            }
            let metadata = PyDict::new(py);
            metadata.set_item("span", occurrence.span)?;
            metadata.set_item(resolution::KEY, resolution.as_str())?;
            metadata.set_item("access", "read")?;
            graph.note_site_file(
                &occurrence.enclosing_id,
                &relative_to(path, project_root),
                &metadata,
            )?;
            let relation = Relation {
                source_id: occurrence.enclosing_id.clone(),
                target_id: target.clone(),
                kind: RelationKind::References,
                metadata: metadata.unbind(),
            };
            graph.push_relation(Py::new(py, relation)?);
            continue;
        }
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
        }

        // `sort.Slice(x, less)`: the library calls what it is given, unless the
        // callee is a function of the project; the flow pass knows which.
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
        // The key of a keyed struct literal: kept when it names a field of the
        // project (an ATTRIBUTE) or a field of an external struct (positional
        // external symbol, like a selector write). A local, parameter, constant
        // or field without a node (map key, function-local type) is dropped.
        let is_literal_key =
            occurrence.role == "write" && occurrence.detail.as_deref() == Some("key");
        // A bare identifier (`name:<id of the package-level node it may denote>`):
        // kept only when go/types binds it to exactly that declaration. A local, a
        // parameter, a builtin or no answer is dropped, with no EXTERNAL_SYMBOL.
        let name_use = occurrence.detail.as_deref().and_then(|d| d.strip_prefix("name:"));
        if let Some(expected) = name_use {
            if target_id.as_deref() != Some(expected) {
                continue;
            }
        } else if is_literal_key {
            let keep = match target_id.as_deref() {
                Some(id) => graph.node_kind(id) == Some(NodeKind::Attribute),
                None => reference.origin != "internal",
            };
            if !keep {
                continue;
            }
        } else if occurrence.role == "call" {
            flow.call(path, occurrence, target_id.as_deref());
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
        // `HandlerFunc(g)`, `T(x)`: a type "called" is a conversion, not a call.
        if rel_kind == RelationKind::Calls
            && matches!(bound_kind, Some(NodeKind::Class | NodeKind::TypeAlias))
        {
            continue;
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
fn apply_boundaries(
    py: Python<'_>,
    graph: &mut Graph,
    files: &[FileBoundaries],
) -> PyResult<()> {
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
            let enclosing_id = crate::python::innermost_enclosing_rust(
                candidates,
                reference.line,
                reference.col,
            )
            .unwrap_or_else(|| file_id.clone());
            crate::python::add_boundary_rust(py, graph, &enclosing_id, reference)?;
        }
    }
    Ok(())
}
