//! DEPENDENCY nodes for the packages a project declares.
//!
//! Every adapter already parses its manifests, because import classification
//! needs to know which names are third-party. Turning that set into nodes
//! costs one pass and answers a question the graph could not otherwise
//! answer — "what does this project depend on, as declared" — independently of
//! whether anything actually imports it.

use std::collections::HashSet;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::graph::Graph;
use crate::ids::node_id;
use crate::node::{Node, NodeKind};
use crate::relation::{Relation, RelationKind};

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
