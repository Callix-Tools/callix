//! Оркестрация анализа Go-проекта.
//!
//! Схема узлов отличается от Python и TypeScript: MODULE — это пакет Go
//! (каталог), а не точечное имя, и его qualified_name совпадает с путём
//! импорта, поэтому внутренние импорты связываются прямым поиском.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::boundaries::BoundaryRef;
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
use super::deps::{go_roots, is_go, module_path, required_modules};
use super::resolver::GoResolver;
use super::visitor::GoExtractor;

const GO_EXTENSIONS: [&str; 1] = [".go"];

type FileBoundaries = (String, String, Vec<BoundaryRef>);
type BuiltRoot = (String, Vec<(String, OccurrenceRef)>, Vec<FileBoundaries>);

/// Исходники Go под корнем.
///
/// Исключается общий список служебных каталогов, а НЕ тот, по которому
/// ищутся корни модулей: `vendor` и `testdata` скрывают вложенные go.mod
/// от поиска корней, но их собственные файлы в анализ входят — так же,
/// как в graphlens, где сбор файлов идёт базовой реализацией.
pub fn collect_go_files(root: &Path) -> Vec<PathBuf> {
    collect_files(root, &GO_EXTENSIONS, &EXCLUDED_DIRS)
}

/// Полное имя пакета: путь модуля плюс каталог файла внутри него.
fn package_qname(file: &Path, go_root: &Path, module_path: &str) -> String {
    let Some(parent) = file.parent() else {
        return module_path.to_string();
    };
    match parent.strip_prefix(go_root) {
        // Файл вне корня модуля (передан явно) — считаем корневым пакетом.
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
        file_path: None,
        span: None,
        metadata: PyDict::new(py).unbind(),
    };
    graph.insert_node(id.clone(), Py::new(py, node)?);
    push_relation(py, graph, project_id.to_string(), id.clone(), RelationKind::Contains)?;
    packages.insert(pkg_qname.to_string(), id.clone());
    Ok(id)
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
    let file_rel = file
        .strip_prefix(project_root)
        .or_else(|_| file.strip_prefix(go_root))
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

/// Структура и импорты одного корня Go-модуля.
fn build_root_structure(
    py: Python<'_>,
    graph: &mut Graph,
    project_root: &Path,
    go_root: &Path,
    files: &[PathBuf],
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
    let required = required_modules(go_root);

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

    let mut packages: IndexMap<String, String> = IndexMap::new();
    let mut occurrences: Vec<(String, OccurrenceRef)> = Vec::new();
    let mut boundaries: Vec<FileBoundaries> = Vec::new();
    let mut internal_imports: Vec<(String, String)> = Vec::new();

    for file in files {
        let pkg_qname = package_qname(file, go_root, &module);
        let module_id = ensure_package(py, graph, &project, &pkg_qname, &project_id, &mut packages)?;
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

    // Внутренние импорты связываем, когда все пакеты уже созданы: путь
    // импорта Go совпадает с полным именем пакета, поэтому достаточно
    // прямого поиска. Не найденное падает в EXTERNAL_SYMBOL, чтобы ребро
    // не пропало.
    for (import_id, import_path) in internal_imports {
        let target_id = match packages.get(&import_path) {
            Some(id) => id.clone(),
            None => ensure_external_symbol(py, graph, &project, &import_path, "internal")?,
        };
        push_relation(py, graph, import_id, target_id, RelationKind::ResolvesTo)?;
    }

    Ok((project, occurrences, boundaries))
}

/// Абсолютный путь резолвера → путь относительно корня проекта.
///
/// Резолвер отдаёт абсолютные пути, а узлы графа хранят относительные;
/// без приведения поиск в SpanIndex промахивается.
fn relative_to(file_path: &str, project_root: &Path) -> String {
    let path = Path::new(file_path);
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    resolved
        .strip_prefix(project_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file_path.to_string())
}

/// Ключ EXTERNAL_SYMBOL для цели, которую не удалось назвать.
///
/// Путь файла входит в ключ намеренно: без него места с одинаковыми
/// координатами в разных файлах схлопывались бы в один узел (см. README,
/// «Отличия от graphlens»).
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

/// Языковой адаптер для Go-проектов.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core", unsendable)]
pub struct GoAdapter {
    resolver: Option<GoResolver>,
}

#[gen_stub_pymethods]
#[pymethods]
impl GoAdapter {
    /// Args:
    ///     resolve: False отключает резолв-фазу — граф останется
    ///         структурным, а `resolver_status` станет `unavailable`.
    #[new]
    #[pyo3(signature = (*, resolve = true))]
    fn new(resolve: bool) -> Self {
        Self {
            resolver: resolve.then(GoResolver::empty),
        }
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

    fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_go_files(&project_root)
    }

    /// Разбирает проект и возвращает граф.
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

        // Фаза 1 — структура по каждому модулю, без резолва.
        let mut built = Vec::with_capacity(root_files.len());
        for (go_root, module_files) in &root_files {
            built.push(build_root_structure(
                py,
                &mut graph,
                &project_root,
                go_root,
                module_files,
            )?);
        }

        // Фаза 2 — один резолвер на весь project_root, чтобы межмодульные
        // вызовы разрешались и воркспейс не перезагружался на каждый модуль.
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

        // Фаза 3 — границы, после резолва.
        for (_project, _occurrences, boundaries) in &built {
            apply_boundaries(py, &mut graph, boundaries)?;
        }

        let status_value = status.as_str().into_pyobject(py)?;
        graph.set_metadata_item(py, RESOLVER_STATUS_KEY, status_value.as_any())?;
        graph.set_metadata_item(py, RESOLVER_METRICS_KEY, metrics.as_dict(py)?.as_any())?;

        if strict && status != ResolverStatus::Ok {
            return Err(AdapterError::new_err(format!(
                "Go resolver status is '{}'; refusing to return a degraded graph in strict mode",
                status.as_str()
            )));
        }
        Ok(graph)
    }

    fn __repr__(&self) -> String {
        format!(
            "GoAdapter(resolver={})",
            if self.resolver.is_some() { "go/packages" } else { "off" }
        )
    }
}

fn resolve_pass(
    py: Python<'_>,
    resolver: &GoResolver,
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
            target_id: target_id.expect("внешний символ создаётся выше"),
            kind: rel_kind,
            metadata: metadata.unbind(),
        };
        graph.push_relation(Py::new(py, relation)?);
    }
    Ok(metrics)
}

/// Узлы BOUNDARY и рёбра EXPOSES / CONSUMES.
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
