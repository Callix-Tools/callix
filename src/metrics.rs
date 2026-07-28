//! Счётчики резолв-фазы, которые адаптер кладёт в метаданные графа.
//!
//! Без них быстрый прогон, не построивший почти ни одного ребра,
//! неотличим от быстрого прогона, который разрешил всё.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

/// Ключ в `graph.metadata`, под которым лежат метрики.
pub const RESOLVER_METRICS_KEY: &str = "resolver_metrics";

#[gen_stub_pyclass]
#[pyclass(module = "callix._core", get_all, set_all, from_py_object)]
#[derive(Default, Clone)]
pub struct ResolverMetrics {
    /// Позиций отдано резолверу — по одной на место использования.
    pub queries: u64,
    /// Запросов, вернувших определение.
    pub resolved: u64,
    /// Определений, привязанных к узлу в графе.
    pub internal: u64,
    /// Определений, ушедших в EXTERNAL_SYMBOL.
    pub external: u64,
    /// Запросов, не давших ничего.
    pub unresolved: u64,
    /// Секунд внутри `resolve_all`.
    pub seconds: f64,
}

#[gen_stub_pymethods]
#[pymethods]
impl ResolverMetrics {
    #[new]
    #[pyo3(signature = (queries = 0, resolved = 0, internal = 0, external = 0, unresolved = 0, seconds = 0.0))]
    fn new(
        queries: u64,
        resolved: u64,
        internal: u64,
        external: u64,
        unresolved: u64,
        seconds: f64,
    ) -> Self {
        Self { queries, resolved, internal, external, unresolved, seconds }
    }

    /// Доля запросов, вернувших определение, в процентах.
    #[getter]
    fn resolved_pct(&self) -> f64 {
        if self.queries == 0 {
            return 0.0;
        }
        100.0 * self.resolved as f64 / self.queries as f64
    }

    /// Складывает счётчики другого прохода в этот.
    pub fn merge(&mut self, other: &Self) {
        self.queries += other.queries;
        self.resolved += other.resolved;
        self.internal += other.internal;
        self.external += other.external;
        self.unresolved += other.unresolved;
        self.seconds += other.seconds;
    }

    /// Плоский словарь для хранения в `graph.metadata`.
    pub fn as_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(py);
        out.set_item("queries", self.queries)?;
        out.set_item("resolved", self.resolved)?;
        out.set_item("internal", self.internal)?;
        out.set_item("external", self.external)?;
        out.set_item("unresolved", self.unresolved)?;
        out.set_item("seconds", round_to(self.seconds, 3))?;
        out.set_item("resolved_pct", round_to(self.resolved_pct(), 1))?;
        Ok(out)
    }

    fn __repr__(&self) -> String {
        format!(
            "ResolverMetrics(queries={}, resolved={}, internal={}, external={}, unresolved={})",
            self.queries, self.resolved, self.internal, self.external, self.unresolved
        )
    }
}

/// Округление до N знаков — как `round()` в Python (half-to-even).
fn round_to(value: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    let scaled = value * factor;
    let rounded = scaled.round_ties_even();
    rounded / factor
}
