//! Граф ↔ JSON-совместимые структуры.
//!
//! JSON гоняется через штатный модуль `json` — так вывод байт в байт
//! совпадает с graphlens (включая `ensure_ascii=False`), а сам dump
//! всё равно исполняется сишным кодом CPython.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::error::SerializationError;
use crate::node::{Node, NodeKind};
use crate::relation::{Relation, RelationKind};
use crate::serde::{decode_metadata, encode_metadata, span_from_list, span_to_list};
use crate::span::Span;

pub const SCHEMA_VERSION: u32 = 1;

fn required<'py>(data: &Bound<'py, PyDict>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    data.get_item(key)?.ok_or_else(|| {
        pyo3::exceptions::PyKeyError::new_err(format!("missing key {key:?}"))
    })
}

/// Сериализует узел в JSON-совместимый словарь.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn node_to_dict<'py>(py: Python<'py>, node: &Node) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    out.set_item("id", &node.id)?;
    out.set_item("kind", node.kind.as_str())?;
    out.set_item("qualified_name", &node.qualified_name)?;
    out.set_item("name", &node.name)?;
    out.set_item("file_path", &node.file_path)?;
    match node.span {
        Some(span) => out.set_item("span", span_to_list(py, span))?,
        None => out.set_item("span", py.None())?,
    }
    out.set_item("metadata", encode_metadata(py, node.metadata.bind(py))?)?;
    Ok(out)
}

/// Обратное к [`node_to_dict`].
#[gen_stub_pyfunction]
#[pyfunction]
pub fn node_from_dict(py: Python<'_>, data: &Bound<'_, PyDict>) -> PyResult<Node> {
    let kind_raw = required(data, "kind")?.str()?.to_string();
    let kind = NodeKind::parse(&kind_raw).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!("unknown NodeKind: {kind_raw:?}"))
    })?;

    let file_path = data
        .get_item("file_path")?
        .and_then(|v| v.extract::<String>().ok());

    let span: Option<Span> = data.get_item("span")?.as_ref().and_then(span_from_list);

    Ok(Node {
        id: required(data, "id")?.str()?.to_string(),
        kind,
        qualified_name: required(data, "qualified_name")?.str()?.to_string(),
        name: required(data, "name")?.str()?.to_string(),
        file_path,
        span,
        metadata: decode_metadata(py, data.get_item("metadata")?.as_ref())?.unbind(),
    })
}

/// Сериализует ребро в JSON-совместимый словарь.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn relation_to_dict<'py>(
    py: Python<'py>,
    relation: &Relation,
) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    out.set_item("source_id", &relation.source_id)?;
    out.set_item("target_id", &relation.target_id)?;
    out.set_item("kind", relation.kind.as_str())?;
    out.set_item("metadata", encode_metadata(py, relation.metadata.bind(py))?)?;
    Ok(out)
}

/// Обратное к [`relation_to_dict`].
#[gen_stub_pyfunction]
#[pyfunction]
pub fn relation_from_dict(py: Python<'_>, data: &Bound<'_, PyDict>) -> PyResult<Relation> {
    let kind_raw = required(data, "kind")?.str()?.to_string();
    let kind = RelationKind::parse(&kind_raw).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!("unknown RelationKind: {kind_raw:?}"))
    })?;

    Ok(Relation {
        source_id: required(data, "source_id")?.str()?.to_string(),
        target_id: required(data, "target_id")?.str()?.to_string(),
        kind,
        metadata: decode_metadata(py, data.get_item("metadata")?.as_ref())?.unbind(),
    })
}

/// Падает с SerializationError на неподдерживаемой версии схемы.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn ensure_schema_version(data: &Bound<'_, PyDict>) -> PyResult<()> {
    let version = data.get_item("schema_version")?;
    let ok = version
        .as_ref()
        .and_then(|v| v.extract::<u32>().ok())
        .is_some_and(|v| v == SCHEMA_VERSION);
    if ok {
        return Ok(());
    }
    let shown = match version {
        Some(v) => v.repr()?.to_string(),
        None => "None".to_string(),
    };
    Err(SerializationError::new_err(format!(
        "Unsupported graph schema_version {shown}; expected {SCHEMA_VERSION}"
    )))
}

/// `json.dumps(data, indent=indent, ensure_ascii=False)`.
pub fn json_dumps(
    py: Python<'_>,
    data: &Bound<'_, PyAny>,
    indent: Option<u32>,
) -> PyResult<String> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("indent", indent)?;
    kwargs.set_item("ensure_ascii", false)?;
    py.import("json")?
        .call_method("dumps", (data,), Some(&kwargs))?
        .extract()
}

/// `json.loads(text)`.
pub fn json_loads<'py>(py: Python<'py>, text: &str) -> PyResult<Bound<'py, PyAny>> {
    py.import("json")?.call_method1("loads", (text,))
}

/// Извлекает список словарей по ключу; отсутствующий или не-список — пусто.
pub fn dict_list<'py>(data: &Bound<'py, PyDict>, key: &str) -> PyResult<Vec<Bound<'py, PyDict>>> {
    let Some(value) = data.get_item(key)? else {
        return Ok(Vec::new());
    };
    let Ok(list) = value.cast::<PyList>() else {
        return Ok(Vec::new());
    };
    Ok(list
        .iter()
        .filter_map(|item| item.cast_into::<PyDict>().ok())
        .collect())
}
