//! A deterministic structural diff of two graphs.

use std::collections::BTreeMap;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pyfunction, gen_stub_pymethods};

use crate::graph::Graph;
use crate::node::Node;
use crate::relation::Relation;
use crate::serde::encode_metadata;

/// An edge key: the node pair, the kind, and canonicalized metadata.
///
/// Metadata is part of the key so that (a) two distinct CALLS between the
/// same nodes from different call sites do not collapse into one edge, and
/// (b) a change to an edge's metadata alone still shows up in the diff —
/// symmetric with nodes, which are compared in full.
type RelationKey = (String, String, &'static str, String);

/// A stable string representation of a value, with keys sorted.
fn canonical(value: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(dict) = value.cast::<PyDict>() {
        let mut entries: BTreeMap<String, String> = BTreeMap::new();
        for (key, item) in dict.iter() {
            entries.insert(key.str()?.to_string(), canonical(&item)?);
        }
        let body: Vec<String> = entries.into_iter().map(|(k, v)| format!("{k}:{v}")).collect();
        return Ok(format!("{{{}}}", body.join(",")));
    }
    if let Ok(list) = value.cast::<PyList>() {
        let mut body = Vec::with_capacity(list.len());
        for item in list.iter() {
            body.push(canonical(&item)?);
        }
        return Ok(format!("[{}]", body.join(",")));
    }
    Ok(value.repr()?.to_string())
}

fn relation_key(py: Python<'_>, relation: &Relation) -> PyResult<RelationKey> {
    let encoded = encode_metadata(py, relation.metadata.bind(py))?;
    Ok((
        relation.source_id.clone(),
        relation.target_id.clone(),
        relation.kind.as_str(),
        canonical(encoded.as_any())?,
    ))
}

#[gen_stub_pyclass]
#[pyclass(module = "callix._core", frozen, get_all)]
pub struct GraphDiff {
    pub added_nodes: Vec<Py<Node>>,
    pub removed_nodes: Vec<Py<Node>>,
    pub changed_nodes: Vec<(Py<Node>, Py<Node>)>,
    pub added_relations: Vec<Py<Relation>>,
    pub removed_relations: Vec<Py<Relation>>,
}

#[gen_stub_pymethods]
#[pymethods]
impl GraphDiff {
    /// True when the graphs are structurally identical.
    #[getter]
    fn is_empty(&self) -> bool {
        self.added_nodes.is_empty()
            && self.removed_nodes.is_empty()
            && self.changed_nodes.is_empty()
            && self.added_relations.is_empty()
            && self.removed_relations.is_empty()
    }

    fn __repr__(&self) -> String {
        format!(
            "GraphDiff(+{} nodes, -{} nodes, ~{} nodes, +{} relations, -{} relations)",
            self.added_nodes.len(),
            self.removed_nodes.len(),
            self.changed_nodes.len(),
            self.added_relations.len(),
            self.removed_relations.len()
        )
    }
}

/// The structural diff from `old` to `new`. Ordering within each list is
/// deterministic: nodes are sorted by ID, edges by their key.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn diff_graphs(py: Python<'_>, old: &Graph, new: &Graph) -> PyResult<GraphDiff> {
    let old_nodes = old.node_map();
    let new_nodes = new.node_map();

    let mut added_nodes = Vec::new();
    let mut changed_nodes = Vec::new();
    for (id, node) in new_nodes {
        match old_nodes.get(id) {
            None => added_nodes.push((id, node)),
            Some(previous) => {
                if !previous.get().__eq__(py, node.get())? {
                    changed_nodes.push((id, previous, node));
                }
            }
        }
    }
    let mut removed_nodes: Vec<_> = old_nodes
        .iter()
        .filter(|(id, _)| !new_nodes.contains_key(*id))
        .collect();

    added_nodes.sort_by_key(|(id, _)| *id);
    removed_nodes.sort_by_key(|(id, _)| *id);
    changed_nodes.sort_by_key(|(id, _, _)| *id);

    let mut old_rels: BTreeMap<RelationKey, &Py<Relation>> = BTreeMap::new();
    for rel in old.relation_slice() {
        old_rels.insert(relation_key(py, rel.get())?, rel);
    }
    let mut new_rels: BTreeMap<RelationKey, &Py<Relation>> = BTreeMap::new();
    for rel in new.relation_slice() {
        new_rels.insert(relation_key(py, rel.get())?, rel);
    }

    let added_relations = new_rels
        .iter()
        .filter(|(key, _)| !old_rels.contains_key(*key))
        .map(|(_, rel)| rel.clone_ref(py))
        .collect();
    let removed_relations = old_rels
        .iter()
        .filter(|(key, _)| !new_rels.contains_key(*key))
        .map(|(_, rel)| rel.clone_ref(py))
        .collect();

    Ok(GraphDiff {
        added_nodes: added_nodes.into_iter().map(|(_, n)| n.clone_ref(py)).collect(),
        removed_nodes: removed_nodes.into_iter().map(|(_, n)| n.clone_ref(py)).collect(),
        changed_nodes: changed_nodes
            .into_iter()
            .map(|(_, before, after)| (before.clone_ref(py), after.clone_ref(py)))
            .collect(),
        added_relations,
        removed_relations,
    })
}
