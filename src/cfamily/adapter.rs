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
use std::time::Instant;

use indexmap::IndexMap;
use crate::flow::{Flow, Slot as FlowSlot};
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
use crate::python::{apply_boundaries_rust, apply_resolutions_rust};
use crate::relation::{Relation, RelationKind};
use crate::resolver_slot::{ResolverSlot, custom_prepare, custom_resolve_all, custom_status};
use crate::roots::filter_nested_root_files;
use crate::span_index::SpanIndex;
use crate::status::ResolverStatus;

use super::boundary::extract_boundaries;
use super::deps::dependencies;
use super::detector::{
    C_EXTENSIONS, CPP_EXTENSIONS, c_roots, collect_c_files, collect_cpp_files, cpp_roots, is_c,
    is_cpp, project_name,
};
use super::headers::{DeclLedger, Linkage, claims, include_search_dirs, resolve_include};
use super::resolver::{CFamilyIndex, Outcome, SymbolTable};
use super::visitor::{CFamilyEmitter, FileFacts, FoundOccurrence, Owner, declared_scopes, read_file};
use super::{Dialect, parse_tree};

type FileBoundaries = (String, String, Vec<BoundaryRef>);

/// One file, carried between the four passes.
struct Analysed {
    file_rel: String,
    /// The path as walked. Kept because a custom resolver is asked about
    /// absolute positions, the way the other adapters ask theirs.
    file_abs: String,
    file_id: String,
    facts: FileFacts,
    boundaries: Vec<BoundaryRef>,
    /// The project-relative paths of the files this one includes.
    included: Vec<String>,
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

/// An edge a resolver produced from a use-site.
///
/// It records where it was written, as every other adapter's does — `span`, and
/// `access` on a `REFERENCES` — which is what lets two call sites of the same
/// function stay two edges and what an index (LSIF) needs to place a reference.
#[allow(clippy::too_many_arguments)]
fn push_reference(
    py: Python<'_>,
    graph: &mut Graph,
    source_id: String,
    target_id: String,
    kind: RelationKind,
    occurrence: &FoundOccurrence,
    site_file: &str,
    resolution: Resolution,
    ambiguous: bool,
) -> PyResult<()> {
    let metadata = PyDict::new(py);
    metadata.set_item("span", occurrence.span)?;
    metadata.set_item(resolution::KEY, resolution.as_str())?;
    if ambiguous {
        // Several declarations carried the name and the table has no types to
        // choose between them: the edge is the first of them, and says so.
        metadata.set_item(resolution::AMBIGUOUS_KEY, true)?;
    }
    if occurrence.role == "read" || occurrence.role == "write" {
        metadata.set_item("access", occurrence.role)?;
    }
    graph.note_site_file(&source_id, site_file, &metadata)?;
    let relation = Relation { source_id, target_id, kind, metadata: metadata.unbind() };
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
    declared: Option<HashSet<String>>,
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
    let mut read: Vec<(String, String, FileFacts, Vec<u8>)> = Vec::with_capacity(sources.len());
    for (file, file_rel, source) in &sources {
        let tree = parse_tree(source, dialect)?;
        let facts = read_file(tree.root_node(), source, dialect, file_rel, &records);
        facts.survey(&mut ledger);
        read.push((
            file_rel.clone(),
            file.to_string_lossy().into_owned(),
            facts,
            source.clone(),
        ));
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
    let libraries = declared.unwrap_or_else(|| dependencies(lang_root));
    declare_dependencies(py, graph, &project, &project_id, &libraries)?;

    // MODULE is the DIRECTORY, in both dialects. A C++ namespace groups
    // declarations, but it does not contain files — it spans them — and every
    // other adapter's MODULE is the thing a FILE nests under. Emitting both
    // shapes put `fixture` beside `include` and `src` in one graph with no way
    // to tell which kind a MODULE was. Namespaces are not lost: they are in
    // every qualified name the visitor builds, which is where a scope with no
    // file of its own belongs.
    let mut modules: IndexMap<String, String> = IndexMap::new();

    let known_files: HashSet<String> = read.iter().map(|(rel, _, _, _)| rel.clone()).collect();
    let search_dirs = include_search_dirs(lang_root);

    // FILE nodes, before the emitter runs: an include has to be able to point
    // at a header's FILE node whichever order the files are walked in.
    let mut file_ids: HashMap<String, String> = HashMap::new();
    for (file_rel, _, _, _) in &read {
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
    for (file_rel, file_abs, facts, source) in read {
        let file_id = file_ids[&file_rel].clone();
        emitter.emit(graph, &facts, &file_id)?;

        let tree = parse_tree(&source, dialect)?;
        let mut boundaries = extract_boundaries(tree.root_node(), &source, dialect);
        boundaries.extend(run_extractors(py, extra_extractors, &source, &file_rel)?);

        // The files this one includes, as the project-relative paths their
        // declarations are filed under; an include that leaves the project
        // names no file and is dropped.
        let included: Vec<String> = facts
            .includes
            .iter()
            .filter(|include| include.style != "computed")
            .filter_map(|include| {
                resolve_include(&include.path, include.style == "quoted", &file_rel, &search_dirs, &known_files)
            })
            .collect();

        analysed.push(Analysed { file_rel, file_abs, file_id, facts, boundaries, included });
    }

    Ok((project, analysed))
}

/// The enclosing node for a use-site, whatever owns it.
fn enclosing_id(project: &str, file: &Analysed, owner: &Owner) -> String {
    match owner {
        Owner::File => file.file_id.clone(),
        Owner::Decl { qualified_name, kind } => node_id(project, qualified_name, kind.as_str()),
    }
}

/// Resolves every occurrence against the sources that were parsed.
///
/// The other four adapters ask a backend "what is defined at this position?"
/// and map the answer back through a `SpanIndex`. There is no such backend
/// here, so the direction is reversed: the index is keyed by name, and
/// `#include` reachability decides which declaration of that name a file means.
fn resolve_with_table(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    analysed: &[Analysed],
) -> PyResult<ResolverMetrics> {
    let mut index = CFamilyIndex::new();

    // (function, name) → a parameter or a called local of that function. Reached
    // through the function it belongs to and never by its bare name: `cb` is a
    // different parameter in every function that has one.
    let mut parameters: std::collections::HashMap<(String, String), (String, NodeKind)> =
        std::collections::HashMap::new();
    // A class's fields in declaration order, for the positional elements of an
    // initialiser: `struct ops o = { g };` fills the first field.
    let mut fields_by_class: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();

    for file in analysed {
        for decl in &file.facts.declarations {
            if decl.kind == NodeKind::Parameter || (decl.kind == NodeKind::Variable && decl.record_kind == "local") {
                if let Owner::Decl { qualified_name, .. } = &decl.owner {
                    parameters.insert(
                        (qualified_name.clone(), decl.name.clone()),
                        (decl.qualified_name.clone(), decl.kind),
                    );
                }
                continue;
            }
            if decl.kind == NodeKind::Attribute
                && let Owner::Decl { qualified_name, kind: NodeKind::Class } = &decl.owner
            {
                fields_by_class.entry(qualified_name.clone()).or_default().push(decl.qualified_name.clone());
            }
            index.declare(
                &decl.name,
                &decl.qualified_name,
                decl.kind,
                &file.file_rel,
                decl.linkage == Linkage::Internal,
            );
            // Classes, their members and fields: what a receiver's type is asked.
            let without_signature = decl.qualified_name.split('(').next().unwrap_or(&decl.qualified_name);
            match decl.kind {
                NodeKind::Class => index.declare_class(&decl.qualified_name),
                NodeKind::Method => {
                    if let Some((class, member)) = without_signature.rsplit_once("::") {
                        index.declare_member(class, member, &decl.qualified_name, decl.kind);
                    }
                }
                NodeKind::Attribute => {
                    if let Some((class, field)) = without_signature.rsplit_once("::") {
                        index.declare_field(class, field, &decl.qualified_name, &decl.type_text);
                    }
                }
                _ => {}
            }
        }
        for occurrence in &file.facts.occurrences {
            if occurrence.role == "base"
                && let Owner::Decl { qualified_name, kind: NodeKind::Class } = &occurrence.owner
            {
                index.add_base(qualified_name, &occurrence.name);
            }
        }
        for included in &file.included {
            index.include(&file.file_rel, included);
        }
    }
    index.close_includes();

    let mut metrics = ResolverMetrics::default();
    let mut flow = Flow::default();
    for file in analysed {
        for occurrence in &file.facts.occurrences {
            // What a variable or a member is initialised with feeds the flow pass;
            // it is no edge of its own. Only a function is worth following.
            if occurrence.role == "alias" {
                if let Some((qualified_name, NodeKind::Function | NodeKind::Method, _)) =
                    index.lookup(&occurrence.name, &file.file_rel)
                {
                    let target = node_id(project, &qualified_name, NodeKind::Function.as_str());
                    let target = if graph.has_node(&target) {
                        target
                    } else {
                        node_id(project, &qualified_name, NodeKind::Method.as_str())
                    };
                    // Who holds it: the field a positional initialiser element fills,
                    // the variable an assignment names, or the owner of the site.
                    let holder = if let Some((class, position)) = &occurrence.site.initializer_member {
                        fields_by_class
                            .iter()
                            .find(|(q, _)| q.as_str() == class || q.ends_with(&format!("::{class}")))
                            .and_then(|(_, fields)| fields.get(*position))
                            .map(|field| node_id(project, field, NodeKind::Attribute.as_str()))
                    } else if let Some(name) = &occurrence.site.assigned_to {
                        let local = match &occurrence.owner {
                            Owner::Decl { qualified_name, .. } => parameters.get(&(qualified_name.clone(), name.clone())),
                            Owner::File => None,
                        };
                        match local {
                            Some((qualified, kind)) => Some(node_id(project, qualified, kind.as_str())),
                            None => match index.lookup(name, &file.file_rel) {
                                Some((qualified, NodeKind::Variable, _)) => {
                                    Some(node_id(project, &qualified, NodeKind::Variable.as_str()))
                                }
                                _ => None,
                            },
                        }
                    } else {
                        Some(enclosing_id(project, file, &occurrence.owner))
                    };
                    if let Some(holder) = holder
                        && graph.has_node(&holder)
                        && graph.has_node(&target)
                    {
                        flow.alias(&holder, &target);
                    }
                }
                continue;
            }
            // A name handed to a call: a reference to the function, and a binding
            // of the call's parameter for the flow pass. Anything that is not a
            // function (a local, a variable) says nothing and is dropped.
            if let Some((line, col, position)) = occurrence.site.argument_of {
                let enclosing = enclosing_id(project, file, &occurrence.owner);
                // A parameter of the enclosing function handed on: the chain that
                // lets a callback travel down several functions.
                let own_parameter = match &occurrence.owner {
                    Owner::Decl { qualified_name, .. } => {
                        parameters.get(&(qualified_name.clone(), occurrence.name.clone()))
                    }
                    Owner::File => None,
                };
                let resolved = match own_parameter {
                    Some((parameter, kind)) => Some((parameter.clone(), *kind)),
                    None => match index.lookup(&occurrence.name, &file.file_rel) {
                        Some((qualified_name, kind @ (NodeKind::Function | NodeKind::Method), _)) => {
                            Some((qualified_name, kind))
                        }
                        _ => None,
                    },
                };
                // Anything else - a local, a variable - says nothing and is dropped.
                let Some((qualified_name, target_kind)) = resolved else {
                    continue;
                };
                let target_id = node_id(project, &qualified_name, target_kind.as_str());
                if !graph.has_node(&target_id) || !graph.has_node(&enclosing) {
                    continue;
                }
                metrics.queries += 1;
                metrics.resolved += 1;
                metrics.internal += 1;
                flow.arg_at(&file.file_rel, line, col, FlowSlot::Index(position), &target_id);
                // A reference to a function; a parameter or a local handed on is no
                // reference worth an edge.
                if !matches!(target_kind, NodeKind::Parameter | NodeKind::Variable) {
                    push_reference(
                        py, graph, enclosing, target_id, RelationKind::References, occurrence, &file.file_rel,
                        Resolution::Name, false,
                    )?;
                }
                continue;
            }
            let Some(mut kind) = role_to_kind(occurrence.role) else {
                continue;
            };
            metrics.queries += 1;

            let enclosing_id = enclosing_id(project, file, &occurrence.owner);
            if !graph.has_node(&enclosing_id) {
                metrics.unresolved += 1;
                continue;
            }

            // A bare name that is a parameter of the function it is written in means
            // that parameter, not whichever one of the project carries the name.
            let bare = occurrence.site.receiver_name.is_none()
                && occurrence.site.qualifier.is_empty()
                && !occurrence.site.constructs
                && !occurrence.site.in_initializer_list;
            let own_parameter = match &occurrence.owner {
                Owner::Decl { qualified_name, .. } if bare && occurrence.role == "call" => {
                    parameters.get(&(qualified_name.clone(), occurrence.name.clone()))
                }
                _ => None,
            };
            if let Some((parameter, parameter_kind)) = own_parameter {
                let target_id = node_id(project, parameter, parameter_kind.as_str());
                if graph.has_node(&target_id) {
                    metrics.resolved += 1;
                    metrics.internal += 1;
                    metrics.to_value += 1;
                    flow.call_at(&file.file_rel, occurrence.span.start_line, occurrence.span.start_col, Some(&target_id));
                    push_reference(
                        py, graph, enclosing_id, target_id, kind, occurrence, &file.file_rel,
                        Resolution::Value, false,
                    )?;
                    continue;
                }
            }
            // A call, a construction and a base in an initialiser list are asked of
            // the class table with everything the site says; the rest by name.
            let asked = if occurrence.role == "call" || occurrence.site.in_initializer_list {
                match index.resolve(&occurrence.name, occurrence.role, &occurrence.site, &file.file_rel) {
                    Outcome::Found { qualified_name, kind, ambiguous } => Some(Some((qualified_name, kind, ambiguous))),
                    // Nothing the project declares: a library, a built-in. A
                    // construction of one is no edge at all; a call is an external one.
                    Outcome::Outside if occurrence.site.constructs => {
                        metrics.queries -= 1;
                        continue;
                    }
                    Outcome::Outside => Some(None),
                    Outcome::NotACall => None,
                }
            } else {
                None
            };
            let answer = match asked {
                Some(answer) => answer,
                None => index.lookup(&occurrence.name, &file.file_rel),
            };
            // A base in an initialiser list is written like a field and is a call of
            // the base's constructor.
            if occurrence.site.in_initializer_list
                && matches!(answer, Some((_, NodeKind::Method | NodeKind::Class, _)))
            {
                kind = RelationKind::Calls;
            }
            match answer {
                Some((qualified_name, target_kind, ambiguous)) => {
                    let target_id = node_id(project, &qualified_name, target_kind.as_str());
                    if !graph.has_node(&target_id) {
                        metrics.unresolved += 1;
                        continue;
                    }
                    metrics.resolved += 1;
                    metrics.internal += 1;
                    // Matched by name: the table has no types. A call that lands on
                    // a variable (a function pointer) found no callee at all.
                    let resolution = match graph.node_kind(&target_id) {
                        Some(NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute)
                            if kind == RelationKind::Calls =>
                        {
                            metrics.to_value += 1;
                            Resolution::Value
                        }
                        _ => Resolution::Name,
                    };
                    if occurrence.role == "call" {
                        flow.call_at(&file.file_rel, occurrence.span.start_line, occurrence.span.start_col, Some(&target_id));
                    }
                    push_reference(
                        py, graph, enclosing_id, target_id, kind, occurrence, &file.file_rel,
                        resolution, ambiguous,
                    )?;
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
                    push_reference(
                        py, graph, enclosing_id, target_id, kind, occurrence, &file.file_rel,
                        Resolution::External, false,
                    )?;
                }
            }
        }
    }
    flow.finish(py, graph)?;
    Ok(metrics)
}

/// Resolves through a caller's own resolver, position by position.
///
/// A custom resolver is asked the question the other four adapters ask theirs —
/// "what is defined at this position?" — because a caller who has gone to the
/// trouble of wiring up `scip-clang` or clangd has positions to answer with, and
/// the shared `apply_resolutions_rust` already knows how to turn those answers
/// into edges. The built-in table cannot be driven this way, which is the whole
/// reason for the two paths.
///
/// Use-sites whose enclosing node is missing from the graph are dropped rather
/// than sent, so no edge can hang off a node that does not exist; they are still
/// counted, as unresolved.
fn resolve_with_custom(
    py: Python<'_>,
    resolver: &Py<PyAny>,
    graph: &mut Graph,
    span_index: &mut SpanIndex,
    project: &str,
    project_root: &Path,
    analysed: &[Analysed],
) -> PyResult<ResolverMetrics> {
    let mut occurrences: Vec<(String, OccurrenceRef)> = Vec::new();
    let mut dropped = 0u64;
    for file in analysed {
        for occurrence in &file.facts.occurrences {
            if role_to_kind(occurrence.role).is_none() {
                continue;
            }
            let enclosing = enclosing_id(project, file, &occurrence.owner);
            if !graph.has_node(&enclosing) {
                dropped += 1;
                continue;
            }
            occurrences.push((
                file.file_abs.clone(),
                OccurrenceRef {
                    role: occurrence.role.to_string(),
                    line: occurrence.span.start_line,
                    col: occurrence.span.start_col,
                    enclosing_id: enclosing,
                    span: occurrence.span,
                    detail: None,
                },
            ));
        }
    }

    let started = Instant::now();
    let refs = custom_resolve_all(py, resolver, &occurrences)?;
    let elapsed = started.elapsed().as_secs_f64();

    let mut metrics = apply_resolutions_rust(
        py,
        graph,
        project,
        project_root,
        span_index,
        &occurrences,
        &refs,
    )?;
    metrics.seconds = elapsed;
    metrics.queries += dropped;
    metrics.unresolved += dropped;
    Ok(metrics)
}

/// The body both adapters share.
#[allow(clippy::too_many_arguments)]
fn analyze_family(
    py: Python<'_>,
    dialect: Dialect,
    resolver: &mut ResolverSlot<SymbolTable>,
    dep_parsers: Option<&Py<PyAny>>,
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
            custom_declared(py, dep_parsers, lang_root)?,
            extractors,
        )?);
    }

    let mut metrics = ResolverMetrics::default();
    let mut status = ResolverStatus::Unavailable;
    match resolver {
        ResolverSlot::Disabled => {}
        ResolverSlot::Native(SymbolTable) => {
            for (project, analysed) in &built {
                let pass = resolve_with_table(py, &mut graph, project, analysed)?;
                metrics.merge(&pass);
            }
            // A symbol table, never a type checker: see resolver.rs for the
            // reasoning and for what it cannot answer.
            status = ResolverStatus::Degraded;
        }
        ResolverSlot::Custom(object) => {
            let all_files: Vec<PathBuf> =
                root_files.iter().flat_map(|(_, files)| files.clone()).collect();
            custom_prepare(py, object, &project_root, &all_files)?;
            let mut span_index = SpanIndex::from_graph(py, &graph)?;
            for (project, analysed) in &built {
                let pass = resolve_with_custom(
                    py,
                    object,
                    &mut graph,
                    &mut span_index,
                    project,
                    &project_root,
                    analysed,
                )?;
                metrics.merge(&pass);
            }
            status = custom_status(py, object)?;
        }
    }

    for (_project, analysed) in &built {
        let boundaries: Vec<FileBoundaries> = analysed
            .iter()
            .filter(|file| !file.boundaries.is_empty())
            .map(|file| (file.file_rel.clone(), file.file_id.clone(), file.boundaries.clone()))
            .collect();
        apply_boundaries_rust(py, &mut graph, &boundaries)?;
    }

    graph.set_resolver_status(py, status)?;

    graph.set_resolver_metrics(py, &metrics)?;

    if strict && status != ResolverStatus::Ok {
        // The hint only holds for the built-in table, which can never report
        // `ok`. A caller who passed their own resolver already knows what it is,
        // and telling them the family has no resolver would be wrong.
        let hint = match resolver {
            ResolverSlot::Native(_) => {
                " The C family has no compiler-backed resolver — see the docs for why."
            }
            _ => "",
        };
        return Err(AdapterError::new_err(format!(
            "{} resolver status is '{}'; refusing to return a degraded graph in strict mode.{}",
            dialect.as_str(),
            status.as_str(),
            hint
        )));
    }

    graph.dedupe_structural_relations(py);
    Ok(graph)
}

/// The C adapter.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct CAdapter {
    resolver: ResolverSlot<SymbolTable>,
    dep_parsers: Option<Py<PyAny>>,
    boundary_extractors: Option<Py<PyAny>>,
}

#[gen_stub_pymethods]
#[pymethods]
impl CAdapter {
    /// Args:
    ///     resolve: False turns the resolution phase off — the graph stays
    ///         structural and `resolver_status` becomes `unavailable`.
    ///     resolver: a custom symbol resolver, replacing the built-in symbol
    ///         table. An object with `prepare(root, files)`,
    ///         `resolve_all(queries)` and `status()`. This is where a
    ///         `compile_commands.json`-based indexer belongs: it is asked about
    ///         positions, while the built-in table resolves by name.
    ///     dep_parsers: custom manifest parsers, replacing the built-in CMake,
    ///         vcpkg, conan and meson readers. Each is an object with
    ///         `can_parse(root)` and `parse(root)`.
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
            resolver: ResolverSlot::new(resolve, resolver, || Ok(SymbolTable))?,
            dep_parsers,
            boundary_extractors,
        })
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

    pub fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_c_files(&project_root)
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
        analyze_family(
            py,
            Dialect::C,
            &mut self.resolver,
            self.dep_parsers.as_ref(),
            self.boundary_extractors.as_ref(),
            project_root,
            files,
            strict,
        )
    }

    fn __repr__(&self) -> String {
        format!("CAdapter(resolver={})", self.resolver.label("symbol table"))
    }
}

/// The C++ adapter.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct CppAdapter {
    resolver: ResolverSlot<SymbolTable>,
    dep_parsers: Option<Py<PyAny>>,
    boundary_extractors: Option<Py<PyAny>>,
}

#[gen_stub_pymethods]
#[pymethods]
impl CppAdapter {
    /// Args:
    ///     resolve: False turns the resolution phase off — the graph stays
    ///         structural and `resolver_status` becomes `unavailable`.
    ///     resolver: a custom symbol resolver, replacing the built-in symbol
    ///         table. An object with `prepare(root, files)`,
    ///         `resolve_all(queries)` and `status()`. This is where a
    ///         `compile_commands.json`-based indexer belongs: it is asked about
    ///         positions, while the built-in table resolves by name.
    ///     dep_parsers: custom manifest parsers, replacing the built-in CMake,
    ///         vcpkg, conan and meson readers. Each is an object with
    ///         `can_parse(root)` and `parse(root)`.
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
            resolver: ResolverSlot::new(resolve, resolver, || Ok(SymbolTable))?,
            dep_parsers,
            boundary_extractors,
        })
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

    pub fn collect_files(&self, project_root: PathBuf) -> Vec<PathBuf> {
        collect_cpp_files(&project_root)
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
        analyze_family(
            py,
            Dialect::Cpp,
            &mut self.resolver,
            self.dep_parsers.as_ref(),
            self.boundary_extractors.as_ref(),
            project_root,
            files,
            strict,
        )
    }

    fn __repr__(&self) -> String {
        format!("CppAdapter(resolver={})", self.resolver.label("symbol table"))
    }
}
