//! The resolver status an adapter records in the graph metadata.
//!
//! It lets the caller tell a structural graph from a complete one: without
//! it, missing CALLS/REFERENCES/HAS_TYPE is indistinguishable from "there
//! genuinely are none".

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass_enum, gen_stub_pymethods};

/// The `graph.metadata` key the status is stored under.
pub const RESOLVER_STATUS_KEY: &str = "resolver_status";

#[gen_stub_pyclass_enum]
#[pyclass(module = "callix._core", eq, eq_int, frozen, hash, from_py_object)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ResolverStatus {
    #[pyo3(name = "OK")]
    Ok,
    #[pyo3(name = "DEGRADED")]
    Degraded,
    #[pyo3(name = "UNAVAILABLE")]
    Unavailable,
}

impl ResolverStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "ok" => Self::Ok,
            "degraded" => Self::Degraded,
            "unavailable" => Self::Unavailable,
            _ => return None,
        })
    }

    /// Worse means larger — used to pick the worst status when merging.
    fn severity(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Degraded => 1,
            Self::Unavailable => 2,
        }
    }

    /// Coerces a stored value back into a status, tolerating garbage.
    pub fn coerce(value: &Bound<'_, PyAny>, default: Self) -> Self {
        if let Ok(status) = value.extract::<Self>() {
            return status;
        }
        value
            .str()
            .ok()
            .and_then(|s| Self::parse(&s.to_string()))
            .unwrap_or(default)
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl ResolverStatus {
    #[getter]
    fn value(&self) -> &'static str {
        self.as_str()
    }

    /// The worst of the given statuses; an empty list is OK.
    #[staticmethod]
    pub fn combine(statuses: Vec<ResolverStatus>) -> ResolverStatus {
        statuses
            .into_iter()
            .max_by_key(|s| s.severity())
            .unwrap_or(ResolverStatus::Ok)
    }

    /// Lenient parsing: an unrecognized value yields `default`
    /// (UNAVAILABLE unless stated otherwise) rather than a ValueError — a
    /// foreign or hand-edited graph must not break reading.
    #[staticmethod]
    #[pyo3(signature = (value, default = None))]
    fn from_value(value: &Bound<'_, PyAny>, default: Option<ResolverStatus>) -> ResolverStatus {
        Self::coerce(value, default.unwrap_or(ResolverStatus::Unavailable))
    }

    fn __str__(&self) -> String {
        format!("ResolverStatus.{}", self.as_str().to_uppercase())
    }
}
