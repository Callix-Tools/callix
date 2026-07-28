//! Накопитель графа и поверхность запросов к нему.
//!
//! Индексы входящих/исходящих рёбер строятся лениво и сбрасываются при
//! добавлении ребра — как в graphlens, но хранят позиции в `relations`,
//! а не копии самих рёбер.

use std::collections::HashMap;

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

    /// Разворачивает ID в узлы, молча пропуская отсутствующие.
    fn resolve(&self, py: Python<'_>, ids: impl Iterator<Item = String>) -> Vec<Py<Node>> {
        ids.filter_map(|id| self.nodes.get(&id).map(|n| n.clone_ref(py)))
            .collect()
    }

    /// Узлы в порядке вставки, без копирования наружу.
    pub(crate) fn node_map(&self) -> &IndexMap<String, Py<Node>> {
        &self.nodes
    }

    pub(crate) fn relation_slice(&self) -> &[Py<Relation>] {
        &self.relations
    }

    /// Пустой граф — конструктор для вызова из Rust.
    pub(crate) fn empty(py: Python<'_>) -> Self {
        Self::new(py)
    }

    /// Кладёт значение в метаданные графа.
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

    /// Добавляет узел, молча пропуская дубликат по ID.
    pub(crate) fn insert_node(&mut self, id: String, node: Py<Node>) {
        self.nodes.entry(id).or_insert(node);
    }

    pub(crate) fn push_relation(&mut self, relation: Py<Relation>) {
        self.relations.push(relation);
        self.out_index = None;
        self.in_index = None;
    }

    /// Соответствие `qualified_name` → id для MODULE-узлов.
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

    // -- построение ------------------------------------------------------

    /// Добавляет узел; DuplicateNodeError при совпадении ID.
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

    /// Добавляет ребро и сбрасывает индексы.
    fn add_relation(&mut self, relation: Py<Relation>) {
        self.relations.push(relation);
        self.out_index = None;
        self.in_index = None;
    }

    /// Вливает другой граф.
    ///
    /// `allow_shared=True` разрешает совпадение *идентичных* узлов — так
    /// объединяются межъязыковые графы, где адаптеры разных языков намеренно
    /// порождают один и тот же BOUNDARY-узел. Коллизия двух *разных* узлов
    /// остаётся ошибкой и в этом режиме.
    #[pyo3(signature = (other, *, allow_shared = false))]
    fn merge(&mut self, py: Python<'_>, other: &Self, allow_shared: bool) -> PyResult<()> {
        for (id, node) in &other.nodes {
            if let Some(existing) = self.nodes.get(id) {
                if allow_shared && existing.get().__eq__(py, node.get())? {
                    continue;
                }
                return Err(DuplicateNodeError::new_err(format!(
                    "Node with id '{id}' already exists"
                )));
            }
            self.nodes.insert(id.clone(), node.clone_ref(py));
        }
        self.relations
            .extend(other.relations.iter().map(|r| r.clone_ref(py)));

        // Статус резолвера сливаем по худшему, а не «побеждает последний»,
        // иначе деградировавшая сторона молча замаскируется.
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

    // -- сериализация / diff ---------------------------------------------

    /// Сериализует граф в JSON-совместимый словарь.
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

    /// Сериализует граф в строку JSON.
    #[pyo3(signature = (*, indent = None))]
    fn to_json(&self, py: Python<'_>, indent: Option<u32>) -> PyResult<String> {
        json_dumps(py, self.to_dict(py)?.as_any(), indent)
    }

    /// Восстанавливает граф из вывода `to_dict`.
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

    /// Восстанавливает граф из вывода `to_json`.
    #[staticmethod]
    fn from_json(py: Python<'_>, text: &str) -> PyResult<Self> {
        let data = json_loads(py, text)?;
        let data = data.cast::<PyDict>().map_err(|_| {
            SerializationError::new_err("graph JSON must decode to an object")
        })?;
        Self::from_dict(py, data)
    }

    /// Структурный diff от этого графа (старого) к `other`.
    fn diff(&self, py: Python<'_>, other: &Self) -> PyResult<GraphDiff> {
        diff_graphs(py, self, other)
    }

    // -- рёбра -----------------------------------------------------------

    /// Рёбра, исходящие из `node_id`.
    #[pyo3(signature = (node_id, kind = None))]
    fn outgoing(
        &mut self,
        py: Python<'_>,
        node_id: &str,
        kind: Option<RelationKind>,
    ) -> Vec<Py<Relation>> {
        self.edges(py, node_id, kind, true)
    }

    /// Рёбра, входящие в `node_id`.
    #[pyo3(signature = (node_id, kind = None))]
    fn incoming(
        &mut self,
        py: Python<'_>,
        node_id: &str,
        kind: Option<RelationKind>,
    ) -> Vec<Py<Relation>> {
        self.edges(py, node_id, kind, false)
    }

    // -- запросы ---------------------------------------------------------

    /// Кого вызывает `node_id`.
    fn callees(&mut self, py: Python<'_>, node_id: &str) -> Vec<Py<Node>> {
        let ids: Vec<String> = self
            .edges(py, node_id, Some(RelationKind::Calls), true)
            .iter()
            .map(|r| r.get().target_id.clone())
            .collect();
        self.resolve(py, ids.into_iter())
    }

    /// Кто вызывает `node_id`.
    fn callers(&mut self, py: Python<'_>, node_id: &str) -> Vec<Py<Node>> {
        let ids: Vec<String> = self
            .edges(py, node_id, Some(RelationKind::Calls), false)
            .iter()
            .map(|r| r.get().source_id.clone())
            .collect();
        self.resolve(py, ids.into_iter())
    }

    /// Кто ссылается на `node_id`.
    fn references_to(&mut self, py: Python<'_>, node_id: &str) -> Vec<Py<Node>> {
        let ids: Vec<String> = self
            .edges(py, node_id, Some(RelationKind::References), false)
            .iter()
            .map(|r| r.get().source_id.clone())
            .collect();
        self.resolve(py, ids.into_iter())
    }

    /// Различные узлы в пределах `depth` переходов в любую сторону.
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

    /// Все узлы данного вида.
    fn nodes_by_kind(&self, py: Python<'_>, kind: NodeKind) -> Vec<Py<Node>> {
        self.nodes
            .values()
            .filter(|n| n.get().kind == kind)
            .map(|n| n.clone_ref(py))
            .collect()
    }

    /// Все узлы, объявленные в файле.
    fn nodes_in_file(&self, py: Python<'_>, file_path: &str) -> Vec<Py<Node>> {
        self.nodes
            .values()
            .filter(|n| n.get().file_path.as_deref() == Some(file_path))
            .map(|n| n.clone_ref(py))
            .collect()
    }

    /// Узлы, у которых короткое или полное имя равно `name`.
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

    /// Новый граф из этих узлов и всех инцидентных им рёбер.
    fn subgraph(&self, py: Python<'_>, node_ids: Vec<String>) -> PyResult<Self> {
        let ids: std::collections::HashSet<String> = node_ids.into_iter().collect();
        let mut sub = Self::new(py);
        for id in &ids {
            if let Some(node) = self.nodes.get(id) {
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

    /// Подграф из всех узлов файла и их рёбер.
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
