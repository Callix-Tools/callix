//! Статус резолвера, который адаптер кладёт в метаданные графа.
//!
//! Нужен, чтобы вызывающий отличал структурный граф от полного: без него
//! отсутствие CALLS/REFERENCES/HAS_TYPE неотличимо от «их правда нет».

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass_enum, gen_stub_pymethods};

/// Ключ в `graph.metadata`, под которым лежит статус.
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

    /// Чем хуже, тем больше — для выбора худшего при слиянии.
    fn severity(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Degraded => 1,
            Self::Unavailable => 2,
        }
    }

    /// Приводит хранимое значение обратно к статусу, терпя мусор.
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

    /// Худший статус из переданных; пустой список — OK.
    #[staticmethod]
    pub fn combine(statuses: Vec<ResolverStatus>) -> ResolverStatus {
        statuses
            .into_iter()
            .max_by_key(|s| s.severity())
            .unwrap_or(ResolverStatus::Ok)
    }

    /// Терпимый разбор: нераспознанное значение даёт `default`
    /// (по умолчанию UNAVAILABLE), а не ValueError — чужой или
    /// поправленный руками граф не должен ронять чтение.
    #[staticmethod]
    #[pyo3(signature = (value, default = None))]
    fn from_value(value: &Bound<'_, PyAny>, default: Option<ResolverStatus>) -> ResolverStatus {
        Self::coerce(value, default.unwrap_or(ResolverStatus::Unavailable))
    }

    fn __str__(&self) -> String {
        format!("ResolverStatus.{}", self.as_str().to_uppercase())
    }
}
