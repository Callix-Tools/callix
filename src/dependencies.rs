//! DEPENDENCY nodes for the packages a project declares.
//!
//! Every adapter already parses its manifests, because import classification
//! needs to know which names are third-party. Turning that set into nodes
//! costs one pass and answers a question the graph could not otherwise
//! answer — "what does this project depend on, as declared" — independently of
//! whether anything actually imports it.

use std::collections::HashSet;
use std::path::Path;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::graph::Graph;
use crate::ids::node_id;
use crate::node::{Node, NodeKind};
use crate::relation::{Relation, RelationKind};

/// The names custom manifest parsers declare, when a caller supplied any.
///
/// `None` means no parsers were passed and the adapter should read the manifests
/// it knows. Custom parsers **replace** the built-in reader rather than adding to
/// it: the reason to pass one is normally that the built-in misreads or misses a
/// manifest, and a caller who wants both can call the built-in parser — every
/// one of them is exported — and pass the union.
///
/// The protocol is two methods, the same for every language:
///
/// ```text
/// can_parse(project_root: Path) -> bool
/// parse(project_root: Path) -> Iterable[str]
/// ```
///
/// What a name means is the language's business: a distribution name in Python,
/// a package name in TypeScript, a module path in Go, a crate in Rust, a vendor
/// prefix in PHP, a library in the C family. A parser is written for one
/// language, not for all of them.
pub(crate) fn custom_declared(
    py: Python<'_>,
    parsers: Option<&Py<PyAny>>,
    project_root: &Path,
) -> PyResult<Option<HashSet<String>>> {
    let Some(parsers) = parsers else {
        return Ok(None);
    };
    let root = py
        .import("pathlib")?
        .getattr("Path")?
        .call1((project_root.to_string_lossy().into_owned(),))?;
    let mut names = HashSet::new();
    for parser in parsers.bind(py).try_iter()? {
        let parser = parser?;
        if !parser.call_method1("can_parse", (&root,))?.is_truthy()? {
            continue;
        }
        // Any iterable of names, not a `set`: a parser that reads a lockfile
        // naturally returns a list, and demanding a set is a trap with no
        // benefit — the names go into a set here either way.
        for name in parser.call_method1("parse", (&root,))?.try_iter()? {
            names.insert(name?.extract::<String>()?);
        }
    }
    Ok(Some(names))
}

/// Declares one DEPENDENCY node per name, with DEPENDS_ON from the project.
///
/// Names are emitted in sorted order: the manifests hand back a `HashSet`, and
/// insertion order is part of the graph's value.
pub(crate) fn declare_dependencies(
    py: Python<'_>,
    graph: &mut Graph,
    project: &str,
    project_id: &str,
    names: &HashSet<String>,
) -> PyResult<()> {
    let mut sorted: Vec<&String> = names.iter().collect();
    sorted.sort();

    for name in sorted {
        if name.is_empty() {
            continue;
        }
        let id = node_id(project, name, NodeKind::Dependency.as_str());
        if !graph.has_node(&id) {
            let node = Node {
                id: id.clone(),
                kind: NodeKind::Dependency,
                qualified_name: name.clone(),
                name: name.clone(),
                file_path: None,
                span: None,
                metadata: PyDict::new(py).unbind(),
            };
            graph.insert_node(id.clone(), Py::new(py, node)?);
        }
        let relation = Relation {
            source_id: project_id.to_string(),
            target_id: id,
            kind: RelationKind::DependsOn,
            metadata: PyDict::new(py).unbind(),
        };
        graph.push_relation(Py::new(py, relation)?);
    }
    Ok(())
}
