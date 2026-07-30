//! The graph accumulator and its query surface.
//!
//! The incoming/outgoing edge indices are built lazily and invalidated when
//! an edge is added — as in graphlens, except they store positions into
//! `relations` rather than copies of the edges themselves.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::diffing::{GraphDiff, diff_graphs};
use crate::error::{DuplicateNodeError, SerializationError};
use crate::node::{Node, NodeKind};
use crate::relation::{Relation, RelationKind};
use crate::serde::{decode_metadata, encode_metadata};
use crate::serialization::{
    SCHEMA_VERSION, dict_list, ensure_schema_version, json_dumps, json_loads, node_from_dict,
    node_to_dict, relation_from_dict, relation_to_dict,
};
use crate::status::{RESOLVER_STATUS_KEY, ResolverStatus};

#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct Graph {
    nodes: IndexMap<String, Py<Node>>,
    relations: Vec<Py<Relation>>,
    metadata: Py<PyDict>,
    out_index: Option<HashMap<String, Vec<usize>>>,
    in_index: Option<HashMap<String, Vec<usize>>>,
}

impl Graph {
    fn ensure_index(&mut self) {
        if self.out_index.is_some() {
            return;
        }
        let mut out: HashMap<String, Vec<usize>> = HashMap::new();
        let mut inc: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, rel) in self.relations.iter().enumerate() {
            let rel = rel.get();
            out.entry(rel.source_id.clone()).or_default().push(i);
            inc.entry(rel.target_id.clone()).or_default().push(i);
        }
        self.out_index = Some(out);
        self.in_index = Some(inc);
    }

    fn edges(
        &mut self,
        py: Python<'_>,
        node_id: &str,
        kind: Option<RelationKind>,
        outgoing: bool,
    ) -> Vec<Py<Relation>> {
        self.ensure_index();
        let index = if outgoing { &self.out_index } else { &self.in_index };
        let Some(positions) = index.as_ref().and_then(|i| i.get(node_id)) else {
            return Vec::new();
        };
        positions
            .iter()
            .map(|&i| &self.relations[i])
            .filter(|rel| kind.is_none_or(|k| rel.get().kind == k))
            .map(|rel| rel.clone_ref(py))
            .collect()
    }

    /// Expands IDs into nodes, silently skipping missing ones.
    fn resolve(&self, py: Python<'_>, ids: impl Iterator<Item = String>) -> Vec<Py<Node>> {
        ids.filter_map(|id| self.nodes.get(&id).map(|n| n.clone_ref(py)))
            .collect()
    }

    /// Nodes in insertion order, without copying them out.
    pub(crate) fn node_map(&self) -> &IndexMap<String, Py<Node>> {
        &self.nodes
    }

    pub(crate) fn relation_slice(&self) -> &[Py<Relation>] {
        &self.relations
    }

    /// An empty graph — the constructor for callers on the Rust side.
    pub(crate) fn empty(py: Python<'_>) -> Self {
        Self::new(py)
    }

    /// Stores a value in the graph metadata.
    pub(crate) fn set_metadata_item(
        &self,
        py: Python<'_>,
        key: &str,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        self.metadata.bind(py).set_item(key, value)
    }

    pub(crate) fn has_node(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
    }

    /// Adds a node, silently skipping a duplicate ID.
    pub(crate) fn insert_node(&mut self, id: String, node: Py<Node>) {
        self.nodes.entry(id).or_insert(node);
    }

    pub(crate) fn push_relation(&mut self, relation: Py<Relation>) {
        self.relations.push(relation);
        self.out_index = None;
        self.in_index = None;
    }

    /// Drops exact-duplicate structural edges, keeping the first of each.
    ///
    /// Visitors emit a DECLARES or IMPORTS edge per *occurrence* while the
    /// node it points at is deduplicated by qualified name (`insert_node`
    /// skips a repeat id). So a local variable assigned eight times produced
    /// one VARIABLE node and eight identical DECLARES edges, and
    /// `from x import a, b, c` produced three identical file→module IMPORTS
    /// edges. On apache/superset that was 10,954 edges, 7.7% of the graph,
    /// none of them carrying anything to tell them apart.
    ///
    /// A structural edge is a fact and has no multiplicity, so the duplicates
    /// are noise: `outgoing(file, IMPORTS)` returned the same module three
    /// times. An edge WITH metadata is a different thing — it records where it
    /// was observed, so two CALLS edges between the same pair are two call
    /// sites and both are kept. Emptiness of the metadata is the test, rather
    /// than a list of kinds, because IMPORTS appears in both roles.
    ///
    /// Runs once at the end of `analyze`, not per insert: the hot path stays
    /// allocation-free and this costs one pass.
    pub(crate) fn dedupe_structural_relations(&mut self, py: Python<'_>) {
        let mut seen: HashSet<(String, String, RelationKind)> = HashSet::new();
        let mut kept: Vec<Py<Relation>> = Vec::with_capacity(self.relations.len());

        for relation in self.relations.drain(..) {
            let keep = {
                let r = relation.get();
                if r.metadata.bind(py).is_empty() {
                    seen.insert((r.source_id.clone(), r.target_id.clone(), r.kind))
                } else {
                    true
                }
            };
            if keep {
                kept.push(relation);
            }
        }

        self.relations = kept;
        self.out_index = None;
        self.in_index = None;
    }

    /// The `qualified_name` → id mapping for MODULE nodes.
    pub(crate) fn module_index(&self) -> HashMap<&str, &str> {
        let mut index = HashMap::new();
        for node in self.nodes.values() {
            let node = node.get();
            if node.kind == NodeKind::Module {
                index
                    .entry(node.qualified_name.as_str())
                    .or_insert(node.id.as_str());
            }
        }
        index
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl Graph {
    #[new]
    fn new(py: Python<'_>) -> Self {
        Self {
            nodes: IndexMap::new(),
            relations: Vec::new(),
            metadata: PyDict::new(py).unbind(),
            out_index: None,
            in_index: None,
        }
    }

    #[getter]
    pub fn nodes(&self, py: Python<'_>) -> IndexMap<String, Py<Node>> {
        self.nodes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone_ref(py)))
            .collect()
    }

    #[getter]
    fn relations(&self, py: Python<'_>) -> Vec<Py<Relation>> {
        self.relations.iter().map(|r| r.clone_ref(py)).collect()
    }

    #[getter]
    fn metadata(&self, py: Python<'_>) -> Py<PyDict> {
        self.metadata.clone_ref(py)
    }

    // -- construction ----------------------------------------------------

    /// Adds a node; raises DuplicateNodeError on an ID collision.
    fn add_node(&mut self, node: Py<Node>) -> PyResult<()> {
        let id = node.get().id.clone();
        if self.nodes.contains_key(&id) {
            return Err(DuplicateNodeError::new_err(format!(
                "Node with id '{id}' already exists"
            )));
        }
        self.nodes.insert(id, node);
        Ok(())
    }

    /// Adds an edge and invalidates the indices.
    fn add_relation(&mut self, relation: Py<Relation>) {
        self.relations.push(relation);
        self.out_index = None;
        self.in_index = None;
    }

    /// Merges another graph in.
    ///
    /// `allow_shared=True` permits *identical* nodes to coincide — that is
    /// how cross-language graphs are combined, where adapters for different
    /// languages deliberately produce the same BOUNDARY node. A collision
    /// between two *different* nodes stays an error even in this mode.
    ///
    /// Either the whole graph merges or nothing does. Relations are appended
    /// without deduplication, so merging the same graph twice doubles its
    /// edges — call it once per source graph.
    #[pyo3(signature = (other, *, allow_shared = false))]
    fn merge(&mut self, py: Python<'_>, other: &Self, allow_shared: bool) -> PyResult<()> {
        // Every id is checked before any is inserted. Validating as it went
        // left a failed merge with part of the other graph already in this
        // one: whether the caller was corrupted depended on where the
        // colliding node happened to sit in insertion order.
        for (id, node) in &other.nodes {
            if let Some(existing) = self.nodes.get(id) {
                if allow_shared && existing.get().__eq__(py, node.get())? {
                    continue;
                }
                return Err(DuplicateNodeError::new_err(format!(
                    "Node with id '{id}' already exists"
                )));
            }
        }
        for (id, node) in &other.nodes {
            if !self.nodes.contains_key(id) {
                self.nodes.insert(id.clone(), node.clone_ref(py));
            }
        }
        self.relations
            .extend(other.relations.iter().map(|r| r.clone_ref(py)));

        // Resolver statuses merge to the worst rather than last-one-wins,
        // otherwise a degraded side would be silently masked.
        let this_meta = self.metadata.bind(py);
        let other_meta = other.metadata.bind(py);
        let before = this_meta.get_item(RESOLVER_STATUS_KEY)?;
        let incoming = other_meta.get_item(RESOLVER_STATUS_KEY)?;
        this_meta.update(other_meta.as_mapping())?;
        if let (Some(before), Some(incoming)) = (before, incoming) {
            let worst = ResolverStatus::combine(vec![
                ResolverStatus::coerce(&before, ResolverStatus::Unavailable),
                ResolverStatus::coerce(&incoming, ResolverStatus::Unavailable),
            ]);
            this_meta.set_item(RESOLVER_STATUS_KEY, worst.as_str())?;
        }

        self.out_index = None;
        self.in_index = None;
        Ok(())
    }

    // -- serialization / diff --------------------------------------------

    /// Serializes the graph into a JSON-compatible dict.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(py);
        out.set_item("schema_version", SCHEMA_VERSION)?;
        out.set_item("metadata", encode_metadata(py, self.metadata.bind(py))?)?;

        let nodes = PyList::empty(py);
        for node in self.nodes.values() {
            nodes.append(node_to_dict(py, node.get())?)?;
        }
        out.set_item("nodes", nodes)?;

        let relations = PyList::empty(py);
        for rel in &self.relations {
            relations.append(relation_to_dict(py, rel.get())?)?;
        }
        out.set_item("relations", relations)?;
        Ok(out)
    }

    /// Serializes the graph into a JSON string.
    #[pyo3(signature = (*, indent = None))]
    fn to_json(&self, py: Python<'_>, indent: Option<u32>) -> PyResult<String> {
        json_dumps(py, self.to_dict(py)?.as_any(), indent)
    }

    /// Rebuilds a graph from `to_dict` output.
    #[staticmethod]
    fn from_dict(py: Python<'_>, data: &Bound<'_, PyDict>) -> PyResult<Self> {
        ensure_schema_version(data)?;
        let mut graph = Self::new(py);
        graph.metadata = decode_metadata(py, data.get_item("metadata")?.as_ref())?.unbind();
        for item in dict_list(data, "nodes")? {
            graph.add_node(Py::new(py, node_from_dict(py, &item)?)?)?;
        }
        for item in dict_list(data, "relations")? {
            graph.add_relation(Py::new(py, relation_from_dict(py, &item)?)?);
        }
        Ok(graph)
    }

    /// Rebuilds a graph from `to_json` output.
    #[staticmethod]
    fn from_json(py: Python<'_>, text: &str) -> PyResult<Self> {
        let data = json_loads(py, text)?;
        let data = data.cast::<PyDict>().map_err(|_| {
            SerializationError::new_err("graph JSON must decode to an object")
        })?;
        Self::from_dict(py, data)
    }

    /// The structural diff from this graph (the old one) to `other`.
    fn diff(&self, py: Python<'_>, other: &Self) -> PyResult<GraphDiff> {
        diff_graphs(py, self, other)
    }

    // -- edges -----------------------------------------------------------

    /// Edges leaving `node_id`.
    #[pyo3(signature = (node_id, kind = None))]
    fn outgoing(
        &mut self,
        py: Python<'_>,
        node_id: &str,
        kind: Option<RelationKind>,
    ) -> Vec<Py<Relation>> {
        self.edges(py, node_id, kind, true)
    }

    /// Edges entering `node_id`.
    #[pyo3(signature = (node_id, kind = None))]
    fn incoming(
        &mut self,
        py: Python<'_>,
        node_id: &str,
        kind: Option<RelationKind>,
    ) -> Vec<Py<Relation>> {
        self.edges(py, node_id, kind, false)
    }

    /// Adds COMMUNICATES_WITH edges across cross-language boundaries.
    ///
    /// Adapters emit a shared BOUNDARY node per contract — its ID comes from
    /// the mechanism and the normalized key alone — with EXPOSES from servers
    /// and CONSUMES from clients. This pass pairs, for every boundary, each
    /// consumer with each provider and adds a directed consumer -> provider
    /// edge. That is the step that turns several single-service graphs, once
    /// merged, into one picture of who calls whom across languages.
    ///
    /// Confidence on the new edge is the product of the two sides': a route
    /// read from a literal on both ends stays 1.0, anything inferred decays.
    /// `min_confidence` drops pairs below a threshold.
    ///
    /// Idempotent — a pair already linked through the same boundary is never
    /// added twice — so it is safe to re-run after re-analysing part of the
    /// graph. Returns the number of edges added.
    #[pyo3(signature = (*, min_confidence = 0.0))]
    fn link_boundaries(&mut self, py: Python<'_>, min_confidence: f64) -> PyResult<usize> {
        let mut seen: HashSet<(String, String, String)> = self
            .relations
            .iter()
            .map(|r| r.get())
            .filter(|r| r.kind == RelationKind::CommunicatesWith)
            .map(|r| {
                let boundary = r
                    .metadata
                    .bind(py)
                    .get_item("boundary_id")
                    .ok()
                    .flatten()
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                (r.source_id.clone(), r.target_id.clone(), boundary)
            })
            .collect();

        let boundaries: Vec<(String, String, String)> = self
            .nodes
            .values()
            .map(|n| n.get())
            .filter(|n| n.kind == NodeKind::Boundary)
            .map(|n| {
                let meta = n.metadata.bind(py);
                let get = |key: &str| {
                    meta.get_item(key)
                        .ok()
                        .flatten()
                        .map(|v| v.to_string())
                        .unwrap_or_default()
                };
                (n.id.clone(), get("mechanism"), get("key"))
            })
            .collect();

        let mut added = 0usize;
        for (boundary_id, mechanism, key) in boundaries {
            let consumers = self.side(py, &boundary_id, RelationKind::Consumes);
            let providers = self.side(py, &boundary_id, RelationKind::Exposes);
            if consumers.is_empty() || providers.is_empty() {
                continue;
            }
            for (consumer, consumer_confidence) in &consumers {
                for (provider, provider_confidence) in &providers {
                    // A service that both exposes and consumes the same
                    // contract is talking to itself; that is not a link.
                    if consumer == provider {
                        continue;
                    }
                    let confidence = consumer_confidence * provider_confidence;
                    if confidence < min_confidence {
                        continue;
                    }
                    let dedupe = (consumer.clone(), provider.clone(), boundary_id.clone());
                    if !seen.insert(dedupe) {
                        continue;
                    }
                    let metadata = PyDict::new(py);
                    metadata.set_item("mechanism", &mechanism)?;
                    metadata.set_item("boundary_id", &boundary_id)?;
                    metadata.set_item("boundary_key", &key)?;
                    metadata.set_item("confidence", confidence)?;
                    let relation = Relation {
                        source_id: consumer.clone(),
                        target_id: provider.clone(),
                        kind: RelationKind::CommunicatesWith,
                        metadata: metadata.unbind(),
                    };
                    self.push_relation(Py::new(py, relation)?);
                    added += 1;
                }
            }
        }
        Ok(added)
    }

    // -- queries ---------------------------------------------------------

    /// What `node_id` calls.
    fn callees(&mut self, py: Python<'_>, node_id: &str) -> Vec<Py<Node>> {
        let ids: Vec<String> = self
            .edges(py, node_id, Some(RelationKind::Calls), true)
            .iter()
            .map(|r| r.get().target_id.clone())
            .collect();
        self.resolve(py, ids.into_iter())
    }

    /// What calls `node_id`.
    fn callers(&mut self, py: Python<'_>, node_id: &str) -> Vec<Py<Node>> {
        let ids: Vec<String> = self
            .edges(py, node_id, Some(RelationKind::Calls), false)
            .iter()
            .map(|r| r.get().source_id.clone())
            .collect();
        self.resolve(py, ids.into_iter())
    }

    /// What references `node_id`.
    fn references_to(&mut self, py: Python<'_>, node_id: &str) -> Vec<Py<Node>> {
        let ids: Vec<String> = self
            .edges(py, node_id, Some(RelationKind::References), false)
            .iter()
            .map(|r| r.get().source_id.clone())
            .collect();
        self.resolve(py, ids.into_iter())
    }

    /// Distinct nodes within `depth` hops in either direction.
    #[pyo3(signature = (node_id, depth = 1))]
    fn neighbors(&mut self, py: Python<'_>, node_id: &str, depth: u32) -> Vec<Py<Node>> {
        let mut seen: std::collections::HashSet<String> =
            std::iter::once(node_id.to_string()).collect();
        let mut frontier = vec![node_id.to_string()];
        let mut found: IndexMap<String, Py<Node>> = IndexMap::new();

        for _ in 0..depth {
            let mut next = Vec::new();
            for id in &frontier {
                let out = self.edges(py, id, None, true);
                let inc = self.edges(py, id, None, false);
                for rel in out.iter().chain(inc.iter()) {
                    let rel = rel.get();
                    let other = if rel.source_id == *id { &rel.target_id } else { &rel.source_id };
                    if !seen.insert(other.clone()) {
                        continue;
                    }
                    next.push(other.clone());
                    if let Some(node) = self.nodes.get(other) {
                        found.insert(other.clone(), node.clone_ref(py));
                    }
                }
            }
            frontier = next;
        }
        found.into_values().collect()
    }

    /// Every node of the given kind.
    fn nodes_by_kind(&self, py: Python<'_>, kind: NodeKind) -> Vec<Py<Node>> {
        self.nodes
            .values()
            .filter(|n| n.get().kind == kind)
            .map(|n| n.clone_ref(py))
            .collect()
    }

    /// Every node declared in the file.
    fn nodes_in_file(&self, py: Python<'_>, file_path: &str) -> Vec<Py<Node>> {
        self.nodes
            .values()
            .filter(|n| n.get().file_path.as_deref() == Some(file_path))
            .map(|n| n.clone_ref(py))
            .collect()
    }

    /// Nodes whose short or qualified name equals `name`.
    fn nodes_by_name(&self, py: Python<'_>, name: &str) -> Vec<Py<Node>> {
        self.nodes
            .values()
            .filter(|n| {
                let n = n.get();
                n.name == name || n.qualified_name == name
            })
            .map(|n| n.clone_ref(py))
            .collect()
    }

    /// A new graph from these nodes and every edge incident to them.
    ///
    /// Nodes are taken in the parent's insertion order, not in the caller's
    /// argument order and emphatically not in a `HashSet`'s: node order is
    /// part of a graph's serialized output, so iterating the set here made
    /// `subgraph(...).to_dict()` differ between processes and unusable as a
    /// cache key, a diff input or a golden file.
    fn subgraph(&self, py: Python<'_>, node_ids: Vec<String>) -> PyResult<Self> {
        let ids: HashSet<String> = node_ids.into_iter().collect();
        let mut sub = Self::new(py);
        // The metadata comes along: a subgraph that reported no
        // `resolver_status` looked like an unresolved graph rather than a slice
        // of a resolved one, which is the opposite of what it is.
        sub.metadata.bind(py).update(self.metadata.bind(py).as_mapping())?;
        for (id, node) in &self.nodes {
            if ids.contains(id) {
                sub.add_node(node.clone_ref(py))?;
            }
        }
        for rel in &self.relations {
            let r = rel.get();
            if !ids.contains(&r.source_id) && !ids.contains(&r.target_id) {
                continue;
            }
            for endpoint in [&r.source_id, &r.target_id] {
                if !sub.nodes.contains_key(endpoint)
                    && let Some(node) = self.nodes.get(endpoint)
                {
                    sub.add_node(node.clone_ref(py))?;
                }
            }
            sub.add_relation(rel.clone_ref(py));
        }
        Ok(sub)
    }

    /// The subgraph of every node in the file, plus their edges.
    fn subgraph_for_file(&self, py: Python<'_>, file_path: &str) -> PyResult<Self> {
        let ids = self
            .nodes
            .values()
            .filter(|n| n.get().file_path.as_deref() == Some(file_path))
            .map(|n| n.get().id.clone())
            .collect();
        self.subgraph(py, ids)
    }

    fn __len__(&self) -> usize {
        self.nodes.len()
    }

    fn __repr__(&self) -> String {
        format!(
            "Graph(nodes={}, relations={})",
            self.nodes.len(),
            self.relations.len()
        )
    }
}

impl Graph {
    /// `(source id, confidence)` for every edge of `kind` entering `node_id`.
    fn side(
        &self,
        py: Python<'_>,
        node_id: &str,
        kind: RelationKind,
    ) -> Vec<(String, f64)> {
        self.relations
            .iter()
            .map(|r| r.get())
            .filter(|r| r.kind == kind && r.target_id == node_id)
            .map(|r| {
                let confidence = r
                    .metadata
                    .bind(py)
                    .get_item("confidence")
                    .ok()
                    .flatten()
                    .and_then(|v| v.extract::<f64>().ok())
                    .unwrap_or(1.0);
                (r.source_id.clone(), confidence)
            })
            .collect()
    }
}
