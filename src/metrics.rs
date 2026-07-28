//! Resolution-pass counters an adapter records in the graph metadata.
//!
//! Without them, a fast run that built almost no edges is indistinguishable
//! from a fast run that resolved everything.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

/// The `graph.metadata` key the metrics are stored under.
pub const RESOLVER_METRICS_KEY: &str = "resolver_metrics";

#[gen_stub_pyclass]
#[pyclass(module = "callix._core", get_all, set_all, from_py_object)]
#[derive(Default, Clone)]
pub struct ResolverMetrics {
    /// Positions handed to the resolver — one per use-site.
    pub queries: u64,
    /// Queries that returned a definition.
    pub resolved: u64,
    /// Definitions bound to a node in the graph.
    pub internal: u64,
    /// Definitions that fell through to an EXTERNAL_SYMBOL.
    pub external: u64,
    /// Queries that returned nothing.
    pub unresolved: u64,
    /// Seconds spent inside `resolve_all`.
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

    /// Share of queries that returned a definition, in percent.
    #[getter]
    fn resolved_pct(&self) -> f64 {
        if self.queries == 0 {
            return 0.0;
        }
        100.0 * self.resolved as f64 / self.queries as f64
    }

    /// Folds another pass's counters into this one.
    pub fn merge(&mut self, other: &Self) {
        self.queries += other.queries;
        self.resolved += other.resolved;
        self.internal += other.internal;
        self.external += other.external;
        self.unresolved += other.unresolved;
        self.seconds += other.seconds;
    }

    /// A flat dict for storage in `graph.metadata`.
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

/// Rounds to N digits the way Python's `round()` does (half-to-even).
fn round_to(value: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    let scaled = value * factor;
    let rounded = scaled.round_ties_even();
    rounded / factor
}
