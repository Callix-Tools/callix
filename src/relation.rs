//! Направленное ребро между двумя узлами, ссылающееся на них по ID.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pyclass_enum, gen_stub_pymethods};

#[gen_stub_pyclass_enum]
#[pyclass(module = "callix._core", eq, eq_int, frozen, hash, from_py_object)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RelationKind {
    #[pyo3(name = "CONTAINS")]
    Contains,
    #[pyo3(name = "DECLARES")]
    Declares,
    #[pyo3(name = "IMPORTS")]
    Imports,
    #[pyo3(name = "CALLS")]
    Calls,
    #[pyo3(name = "REFERENCES")]
    References,
    #[pyo3(name = "DEPENDS_ON")]
    DependsOn,
    #[pyo3(name = "RESOLVES_TO")]
    ResolvesTo,
    #[pyo3(name = "INHERITS_FROM")]
    InheritsFrom,
    #[pyo3(name = "HAS_TYPE")]
    HasType,
    #[pyo3(name = "EXPOSES")]
    Exposes,
    #[pyo3(name = "CONSUMES")]
    Consumes,
    #[pyo3(name = "COMMUNICATES_WITH")]
    CommunicatesWith,
}

impl RelationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Declares => "declares",
            Self::Imports => "imports",
            Self::Calls => "calls",
            Self::References => "references",
            Self::DependsOn => "depends_on",
            Self::ResolvesTo => "resolves_to",
            Self::InheritsFrom => "inherits_from",
            Self::HasType => "has_type",
            Self::Exposes => "exposes",
            Self::Consumes => "consumes",
            Self::CommunicatesWith => "communicates_with",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "contains" => Self::Contains,
            "declares" => Self::Declares,
            "imports" => Self::Imports,
            "calls" => Self::Calls,
            "references" => Self::References,
            "depends_on" => Self::DependsOn,
            "resolves_to" => Self::ResolvesTo,
            "inherits_from" => Self::InheritsFrom,
            "has_type" => Self::HasType,
            "exposes" => Self::Exposes,
            "consumes" => Self::Consumes,
            "communicates_with" => Self::CommunicatesWith,
            _ => return None,
        })
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl RelationKind {
    #[getter]
    fn value(&self) -> &'static str {
        self.as_str()
    }

    #[staticmethod]
    fn from_value(value: &str) -> PyResult<Self> {
        Self::parse(value).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!("unknown RelationKind: {value:?}"))
        })
    }

    fn __str__(&self) -> String {
        format!("RelationKind.{}", self.as_str().to_uppercase())
    }
}

#[gen_stub_pyclass]
#[pyclass(module = "callix._core", frozen, get_all)]
pub struct Relation {
    pub source_id: String,
    pub target_id: String,
    pub kind: RelationKind,
    pub metadata: Py<PyDict>,
}

#[gen_stub_pymethods]
#[pymethods]
impl Relation {
    #[new]
    #[pyo3(signature = (source_id, target_id, kind, metadata = None))]
    fn new(
        py: Python<'_>,
        source_id: String,
        target_id: String,
        kind: RelationKind,
        metadata: Option<Py<PyDict>>,
    ) -> Self {
        Self {
            source_id,
            target_id,
            kind,
            metadata: metadata.unwrap_or_else(|| PyDict::new(py).unbind()),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "Relation({:?} -{}-> {:?})",
            self.source_id,
            self.kind.as_str(),
            self.target_id
        )
    }

    pub fn __eq__(&self, py: Python<'_>, other: &Self) -> PyResult<bool> {
        Ok(self.source_id == other.source_id
            && self.target_id == other.target_id
            && self.kind == other.kind
            && self.metadata.bind(py).eq(other.metadata.bind(py))?)
    }
}
