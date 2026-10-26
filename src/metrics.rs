//! Resolution-pass counters an adapter records in the graph metadata.
//!
//! Without them, a fast run that built almost no edges is indistinguishable
//! from a fast run that resolved everything.

use crate::py::prelude::*;
use crate::py::types::PyDict;
use crate::py::derive::{gen_stub_pyclass, gen_stub_pymethods};

/// The `graph.metadata` key the metrics are stored under.
pub const RESOLVER_METRICS_KEY: &str = "resolver_metrics";

#[gen_stub_pyclass]
#[pyclass(module = "callix._core", get_all, set_all, from_py_object)]
#[derive(Default, Clone)]
pub struct ResolverMetrics {
    /// Positions handed to the resolver — one per use-site.
    pub queries: u64,
    /// Queries the backend answered with a place. That is all it says: the place
    /// may be the callee, a variable the callee was read from, something outside
    /// the project, or a location callix could not bind (see `to_value` and
    /// `unlocated`). It measures that the backend answered, not that it was right.
    pub resolved: u64,
    /// Answers bound to a node in the graph.
    pub internal: u64,
    /// Answers that fell through to an EXTERNAL_SYMBOL: outside the project, or
    /// inside it but not bound to a node (`unlocated`).
    pub external: u64,
    /// Queries the backend did not answer at all.
    pub unresolved: u64,
    /// Seconds spent inside `resolve_all`.
    pub seconds: f64,
    /// Calls whose target is a variable, a parameter or a field: the resolver found
    /// what the callee was read from, not the callee. A subset of `internal`.
    pub to_value: u64,
    /// Answers that could not be bound to a node of the project although the
    /// resolver called them internal, or that pointed inside an import. A subset of
    /// `external`.
    pub unlocated: u64,
}

#[gen_stub_pymethods]
#[pymethods]
impl ResolverMetrics {
    #[new]
    #[pyo3(signature = (queries = 0, resolved = 0, internal = 0, external = 0, unresolved = 0, seconds = 0.0, to_value = 0, unlocated = 0))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        queries: u64,
        resolved: u64,
        internal: u64,
        external: u64,
        unresolved: u64,
        seconds: f64,
        to_value: u64,
        unlocated: u64,
    ) -> Self {
        Self { queries, resolved, internal, external, unresolved, seconds, to_value, unlocated }
    }

    /// Share of queries the backend answered, in percent. Not a measure of
    /// correctness: see `resolved`.
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
        self.to_value += other.to_value;
        self.unlocated += other.unlocated;
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
        out.set_item("to_value", self.to_value)?;
        out.set_item("unlocated", self.unlocated)?;
        out.set_item("resolved_pct", round_to(self.resolved_pct(), 1))?;
        Ok(out)
    }

    fn __repr__(&self) -> String {
        format!(
            "ResolverMetrics(queries={}, resolved={}, internal={}, external={}, unresolved={}, to_value={}, unlocated={})",
            self.queries, self.resolved, self.internal, self.external, self.unresolved, self.to_value, self.unlocated
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
