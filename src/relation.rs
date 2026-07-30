//! A directed edge between two nodes, referring to them by ID.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

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

#[cfg(test)]
mod kind_tests {
    use super::*;

    /// The graph contract is 12 relation kinds. As with `NodeKind`, `as_str`
    /// and `parse` are separate match arms and drift between them is silent.
    const ALL: [RelationKind; 12] = [
        RelationKind::Contains,
        RelationKind::Declares,
        RelationKind::Imports,
        RelationKind::Calls,
        RelationKind::References,
        RelationKind::DependsOn,
        RelationKind::ResolvesTo,
        RelationKind::InheritsFrom,
        RelationKind::HasType,
        RelationKind::Exposes,
        RelationKind::Consumes,
        RelationKind::CommunicatesWith,
    ];

    /// A second, independent spelling of the wire values. Exhaustive on
    /// purpose: a new variant fails to compile until it is written down here
    /// and added to `ALL`.
    fn expected_str(kind: RelationKind) -> &'static str {
        match kind {
            RelationKind::Contains => "contains",
            RelationKind::Declares => "declares",
            RelationKind::Imports => "imports",
            RelationKind::Calls => "calls",
            RelationKind::References => "references",
            RelationKind::DependsOn => "depends_on",
            RelationKind::ResolvesTo => "resolves_to",
            RelationKind::InheritsFrom => "inherits_from",
            RelationKind::HasType => "has_type",
            RelationKind::Exposes => "exposes",
            RelationKind::Consumes => "consumes",
            RelationKind::CommunicatesWith => "communicates_with",
        }
    }

    #[test]
    fn there_are_exactly_twelve_relation_kinds() {
        assert_eq!(ALL.len(), 12);
    }

    #[test]
    fn as_str_and_parse_round_trip_for_every_kind() {
        for kind in ALL {
            assert_eq!(kind.as_str(), expected_str(kind));
            assert_eq!(
                RelationKind::parse(kind.as_str()),
                Some(kind),
                "{} does not round-trip",
                kind.as_str()
            );
        }
    }

    #[test]
    fn every_wire_value_is_distinct() {
        for (i, left) in ALL.iter().enumerate() {
            for right in &ALL[i + 1..] {
                assert_ne!(left.as_str(), right.as_str());
                assert_ne!(left, right);
            }
        }
    }

    #[test]
    fn parse_rejects_an_unknown_value() {
        for value in ["", "CALLS", "dependson", "depends-on", "communicates with"] {
            assert_eq!(RelationKind::parse(value), None, "{value:?} was accepted");
        }
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

    /// Hashes the endpoints and the kind — everything `__eq__` compares except
    /// the metadata dict, which is not hashable. See `Node::__hash__` for why
    /// a partial hash is the right answer rather than no hash at all.
    fn __hash__(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.source_id.hash(&mut hasher);
        self.target_id.hash(&mut hasher);
        self.kind.hash(&mut hasher);
        hasher.finish()
    }
}
