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
use crate::graph::Graph;
use crate::ids::node_id;
use crate::metrics::{RESOLVER_METRICS_KEY, ResolverMetrics};
use crate::node::{Node, NodeKind};
use crate::relation::{Relation, RelationKind};
use crate::roots::{EXCLUDED_DIRS, collect_files};
use crate::status::{RESOLVER_STATUS_KEY, ResolverStatus};

use super::boundary::{Flavour, extract_boundaries, flavour, items, pairs, root_mapping};
use super::detector::{YAML_EXTENSIONS, is_yaml, project_name, yaml_roots};

type FileBoundaries = (String, String, Vec<BoundaryRef>);

pub fn collect_yaml_files(root: &Path) -> Vec<PathBuf> {
    collect_files(root, &YAML_EXTENSIONS, &EXCLUDED_DIRS)
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

/// A MODULE node for one Compose service.
///
/// A service is the closest thing YAML has to a namespace: a deployable unit
/// that owns an image and depends on others. Modelling it as MODULE keeps the
/// `depends_on` wiring expressible with the vocabulary that already exists.
fn ensure_service(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    name: &str,
    project_id: &str,
    file_rel: &str,
    services: &mut IndexMap<String, String>,
) -> PyResult<String> {
    if let Some(id) = services.get(name) {
        return Ok(id.clone());
    }
    let id = node_id(project, name, NodeKind::Module.as_str());
    if !graph.has_node(&id) {
        let metadata = PyDict::new(py);
        metadata.set_item("kind", "compose-service")?;
        let node = Node {
            id: id.clone(),
            kind: NodeKind::Module,
            qualified_name: name.to_string(),
            name: name.to_string(),
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
            ensure_service(py, graph, project, &name, project_id, file_rel, services)?;
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
        let from_id = ensure_service(py, graph, project, &from, project_id, file_rel, services)?;
        let to_id = ensure_service(py, graph, project, &to, project_id, file_rel, services)?;
        push_relation(py, graph, from_id, to_id, RelationKind::DependsOn)?;
    }
    Ok(())
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
            let shape = flavour(root, &source);

            if !graph.has_node(&file_id) {
                let metadata = PyDict::new(py);
                metadata.set_item(
                    "flavour",
                    match shape {
                        Flavour::OpenApi => "openapi",
                        Flavour::Compose => "compose",
                        Flavour::Kubernetes => "kubernetes",
                        Flavour::Unknown => "unknown",
                    },
                )?;
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

            if shape == Flavour::Compose {
                let ctx = FileContext {
                    project: &project,
                    project_id: &project_id,
                    file_rel: &file_rel,
                };
                compose(py, &mut graph, &ctx, root, &source, &mut services)?;
            }

            let mut found = extract_boundaries(root, &source);
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
