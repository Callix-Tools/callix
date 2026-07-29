//! Orchestration of YAML analysis.
//!
//! Two phases rather than three: there is nothing to resolve, so structure and
//! boundaries are all there is. `resolver_status` reports `ok` with zero
//! queries — resolution had nothing to do, which is a different statement from
//! `unavailable`, where it could not run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::boundaries::{BoundaryRef, run_extractors};
use crate::dependencies::declare_dependencies;
use crate::graph::Graph;
use crate::ids::node_id;
use crate::metrics::{RESOLVER_METRICS_KEY, ResolverMetrics};
use crate::node::{Node, NodeKind};
use crate::relation::{Relation, RelationKind};
use crate::roots::{EXCLUDED_DIRS, collect_files};
use crate::status::{RESOLVER_STATUS_KEY, ResolverStatus};

use super::boundary::{
    Flavour, Job, chart_dependencies, extract_boundaries, flavour, includes, items, jobs, pairs,
    refs, root_mapping,
};
use super::detector::{YAML_EXTENSIONS, is_yaml, project_name, yaml_roots};

type FileBoundaries = (String, String, Vec<BoundaryRef>);

/// A vendored or built tree carries plenty of YAML that describes somebody
/// else's project.
const YAML_SKIP_AT_ROOT: [&str; 2] = ["vendor", "target"];

pub fn collect_yaml_files(root: &Path) -> Vec<PathBuf> {
    collect_files(root, &YAML_EXTENSIONS, &EXCLUDED_DIRS, &YAML_SKIP_AT_ROOT)
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

/// A MODULE node for one Compose service or CI job.
///
/// Both are the closest thing YAML has to a namespace: a unit of work that owns
/// its configuration and waits on others. Modelling them as MODULE keeps the
/// wiring expressible with the vocabulary that already exists.
#[allow(clippy::too_many_arguments)]
fn ensure_unit(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    name: &str,
    project_id: &str,
    file_rel: &str,
    unit_kind: &str,
    services: &mut IndexMap<String, String>,
) -> PyResult<String> {
    if let Some(id) = services.get(name) {
        return Ok(id.clone());
    }
    let id = node_id(project, name, NodeKind::Module.as_str());
    if !graph.has_node(&id) {
        let metadata = PyDict::new(py);
        metadata.set_item("kind", unit_kind)?;
        let node = Node {
            id: id.clone(),
            kind: NodeKind::Module,
            qualified_name: name.to_string(),
            name: name.rsplit(':').next().unwrap_or(name).to_string(),
            file_path: Some(file_rel.to_string()),
            span: None,
            metadata: metadata.unbind(),
        };
        graph.insert_node(id.clone(), Py::new(py, node)?);
        push_relation(py, graph, project_id.to_string(), id.clone(), RelationKind::Contains)?;
    }
    services.insert(name.to_string(), id.clone());
    Ok(id)
}

/// One unresolved reference, held until every file node exists.
struct DeferredInclude {
    source_id: String,
    from_path: String,
    target: String,
    external: bool,
    line: u32,
}

/// An include target, made relative to the project root.
///
/// References are written relative to the including file, so `ci/build.yml`
/// inside `deploy/pipeline.yml` means `deploy/ci/build.yml`. `.` and `..` are
/// folded here rather than on the filesystem, because the target may not exist.
fn resolve_include(from_path: &str, target: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !target.starts_with('/') {
        let mut base: Vec<&str> = from_path.split('/').collect();
        base.pop();
        parts.extend(base);
    }
    for segment in target.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// The context one file's analysis needs, so it travels as one value.
struct FileContext<'a> {
    project: &'a str,
    project_id: &'a str,
    file_rel: &'a str,
}

/// Compose services and the `depends_on` edges between them.
fn compose(
    py: Python<'_>,
    graph: &mut Graph,
    ctx: &FileContext<'_>,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    services: &mut IndexMap<String, String>,
) -> PyResult<()> {
    let (project, project_id, file_rel) = (ctx.project, ctx.project_id, ctx.file_rel);
    let mut wiring: Vec<(String, String)> = Vec::new();

    for (key, value) in root_mapping(root, source) {
        if key != "services" {
            continue;
        }
        for (name, definition) in pairs(value, source) {
            ensure_unit(
                py, graph, project, &name, project_id, file_rel, "compose-service", services,
            )?;
            for (field, body) in pairs(definition, source) {
                if field != "depends_on" {
                    continue;
                }
                // Both spellings are valid: a plain list, or a mapping whose
                // keys are the services and whose values hold conditions.
                let listed: Vec<String> = items(body)
                    .into_iter()
                    .map(|n| {
                        crate::ts::text(n, source)
                            .trim()
                            .trim_matches(|c| c == '"' || c == '\'')
                            .to_string()
                    })
                    .collect();
                let mapped: Vec<String> = pairs(body, source).into_iter().map(|(k, _)| k).collect();
                for other in listed.into_iter().chain(mapped) {
                    if !other.is_empty() {
                        wiring.push((name.clone(), other));
                    }
                }
            }
        }
    }

    for (from, to) in wiring {
        let from_id = ensure_unit(
            py, graph, project, &from, project_id, file_rel, "compose-service", services,
        )?;
        let to_id = ensure_unit(
            py, graph, project, &to, project_id, file_rel, "compose-service", services,
        )?;
        push_relation(py, graph, from_id, to_id, RelationKind::DependsOn)?;
    }
    Ok(())
}

/// CI jobs, what they wait on, and the actions they pull in.
fn ci_jobs(
    py: Python<'_>,
    graph: &mut Graph,
    ctx: &FileContext<'_>,
    found: &[Job],
    units: &mut IndexMap<String, String>,
) -> PyResult<Vec<(String, String, u32)>> {
    let (project, project_id, file_rel) = (ctx.project, ctx.project_id, ctx.file_rel);
    let mut local_uses = Vec::new();

    // A job's name is only unique within its pipeline: two workflows may both
    // have a `test`, and they are not the same job.
    let qualify = |name: &str| format!("{file_rel}:{name}");

    for job in found {
        let job_id = ensure_unit(
            py, graph, project, &qualify(&job.name), project_id, file_rel, "ci-job", units,
        )?;
        for other in &job.needs {
            let other_id = ensure_unit(
                py, graph, project, &qualify(other), project_id, file_rel, "ci-job", units,
            )?;
            push_relation(py, graph, job_id.clone(), other_id, RelationKind::DependsOn)?;
        }
        // An external action is a declared dependency, exactly like a package
        // in a manifest — the version is not part of its identity.
        for action in &job.uses_external {
            let id = node_id(project, action, NodeKind::Dependency.as_str());
            if !graph.has_node(&id) {
                let node = Node {
                    id: id.clone(),
                    kind: NodeKind::Dependency,
                    qualified_name: action.clone(),
                    name: action.rsplit('/').next().unwrap_or(action).to_string(),
                    file_path: None,
                    span: None,
                    metadata: PyDict::new(py).unbind(),
                };
                graph.insert_node(id.clone(), Py::new(py, node)?);
            }
            push_relation(py, graph, job_id.clone(), id, RelationKind::DependsOn)?;
        }
        for target in &job.uses_local {
            local_uses.push((job_id.clone(), target.clone(), 0));
        }
    }
    Ok(local_uses)
}

/// The language adapter for YAML: specifications and service wiring.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct YamlAdapter {
    boundary_extractors: Option<Py<PyAny>>,
}

#[gen_stub_pymethods]
#[pymethods]
impl YamlAdapter {
    /// Args:
    ///     resolve: accepted and ignored — YAML has no symbols to resolve, so
    ///         the phase does not exist. Present so the four-adapter interface
    ///         stays uniform.
    ///     boundary_extractors: extra boundary extractors, run in **addition**
    ///         to the built-in ones. Each is an object with
    ///         `extract(source: bytes, file_path: str) -> list[BoundaryRef]`.
    #[new]
    #[pyo3(signature = (*, resolve = true, boundary_extractors = None))]
    fn new(resolve: bool, boundary_extractors: Option<Py<PyAny>>) -> Self {
        let _ = resolve;
        Self { boundary_extractors }
    }

    fn language(&self) -> &'static str {
        "yaml"
    }

    fn file_extensions(&self) -> HashSet<String> {
        YAML_EXTENSIONS.iter().map(|e| (*e).to_string()).collect()
    }

    fn can_handle(&self, project_root: PathBuf) -> bool {
        is_yaml(&project_root)
    }

    fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_yaml_files(&project_root)
    }

    /// Analyses the tree and returns the graph.
    #[pyo3(signature = (project_root, files = None, *, strict = false))]
    fn analyze(
        &mut self,
        py: Python<'_>,
        project_root: PathBuf,
        files: Option<Vec<PathBuf>>,
        strict: bool,
    ) -> PyResult<Graph> {
        let _ = strict;
        let project_root = project_root.canonicalize().unwrap_or(project_root);
        let files = match files {
            Some(files) => files,
            None => yaml_roots(&project_root)
                .iter()
                .flat_map(|root| collect_yaml_files(root))
                .collect(),
        };

        let mut graph = Graph::empty(py);
        let project = project_name(&project_root);
        let project_id = node_id(&project, &project, NodeKind::Project.as_str());
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

        let mut services: IndexMap<String, String> = IndexMap::new();
        let mut boundaries: Vec<FileBoundaries> = Vec::new();
        let mut deferred: Vec<DeferredInclude> = Vec::new();
        let mut files_by_path: IndexMap<String, String> = IndexMap::new();

        for file in &files {
            let file_rel = file
                .strip_prefix(&project_root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| file.to_string_lossy().into_owned());

            let file_id = node_id(&project, &file_rel, NodeKind::File.as_str());
            let Ok(source) = std::fs::read(file) else {
                continue;
            };
            let tree = super::parse_tree(&source)?;
            let root = tree.root_node();
            let shape = flavour(root, &source, &file_rel);

            if !graph.has_node(&file_id) {
                let metadata = PyDict::new(py);
                metadata.set_item("flavour", shape.as_str())?;
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
                    metadata: metadata.unbind(),
                };
                graph.insert_node(file_id.clone(), Py::new(py, node)?);
                push_relation(
                    py,
                    &mut graph,
                    project_id.clone(),
                    file_id.clone(),
                    RelationKind::Contains,
                )?;
            }
            files_by_path.insert(file_rel.clone(), file_id.clone());

            let ctx = FileContext {
                project: &project,
                project_id: &project_id,
                file_rel: &file_rel,
            };
            if shape == Flavour::Compose {
                compose(py, &mut graph, &ctx, root, &source, &mut services)?;
            }
            if shape == Flavour::HelmChart {
                let declared: std::collections::HashSet<String> =
                    chart_dependencies(root, &source).into_iter().collect();
                declare_dependencies(py, &mut graph, &project, &project_id, &declared)?;
            }
            if matches!(shape, Flavour::GitLabCi | Flavour::GitHubActions) {
                let found = jobs(root, &source, shape);
                for (job_id, target, line) in
                    ci_jobs(py, &mut graph, &ctx, &found, &mut services)?
                {
                    deferred.push(DeferredInclude {
                        source_id: job_id,
                        from_path: file_rel.clone(),
                        target,
                        external: false,
                        line,
                    });
                }
            }

            // Includes are resolved in a second pass: the file they point at
            // may not have been walked yet.
            let mut pulled = includes(root, &source, shape);
            if shape == Flavour::OpenApi {
                pulled.extend(refs(root, &source));
            }
            for include in pulled {
                deferred.push(DeferredInclude {
                    source_id: file_id.clone(),
                    from_path: file_rel.clone(),
                    target: include.target,
                    external: include.external,
                    line: include.line,
                });
            }

            let mut found = extract_boundaries(root, &source, &file_rel);
            found.extend(run_extractors(
                py,
                self.boundary_extractors.as_ref(),
                &source,
                &file_rel,
            )?);
            if !found.is_empty() {
                boundaries.push((file_rel, file_id, found));
            }
        }

        // An include either lands on a file in this tree or becomes an
        // external symbol, so the edge is never lost — the same rule the other
        // adapters follow for imports.
        for include in deferred {
            let DeferredInclude { source_id, from_path, target, external, line } = include;
            let resolved = (!external)
                .then(|| resolve_include(&from_path, &target))
                .and_then(|path| files_by_path.get(&path).cloned());
            let target_id = match resolved {
                Some(id) => id,
                None => {
                    let id = node_id(&project, &target, NodeKind::ExternalSymbol.as_str());
                    if !graph.has_node(&id) {
                        let metadata = PyDict::new(py);
                        metadata.set_item(
                            "origin",
                            if external { "third_party" } else { "unknown" },
                        )?;
                        let node = Node {
                            id: id.clone(),
                            kind: NodeKind::ExternalSymbol,
                            qualified_name: target.clone(),
                            name: target.rsplit('/').next().unwrap_or(&target).to_string(),
                            file_path: None,
                            span: None,
                            metadata: metadata.unbind(),
                        };
                        graph.insert_node(id.clone(), Py::new(py, node)?);
                    }
                    id
                }
            };
            let metadata = PyDict::new(py);
            metadata.set_item("include", &target)?;
            metadata.set_item("line", line)?;
            let relation = Relation {
                source_id,
                target_id,
                kind: RelationKind::Imports,
                metadata: metadata.unbind(),
            };
            graph.push_relation(Py::new(py, relation)?);
        }

        for (file_rel, file_id, refs) in &boundaries {
            let _ = file_rel;
            for reference in refs {
                crate::python::add_boundary_rust(py, &mut graph, file_id, reference)?;
            }
        }

        // Nothing to resolve is not the same as resolution being unavailable:
        // the graph is as complete as this language allows.
        let status = ResolverStatus::Ok.as_str().into_pyobject(py)?;
        graph.set_metadata_item(py, RESOLVER_STATUS_KEY, status.as_any())?;
        let metrics = ResolverMetrics::default();
        graph.set_metadata_item(py, RESOLVER_METRICS_KEY, metrics.as_dict(py)?.as_any())?;
        Ok(graph)
    }

    fn __repr__(&self) -> String {
        "YamlAdapter(specifications and service wiring; no resolver)".to_string()
    }
}
