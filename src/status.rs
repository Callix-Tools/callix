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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant. `as_str`, `parse` and `severity` are three independent
    /// match arms over the same enum, and a status added to one but not the
    /// others is exactly the kind of silent drift no Python test can see.
    const ALL: [ResolverStatus; 3] = [
        ResolverStatus::Ok,
        ResolverStatus::Degraded,
        ResolverStatus::Unavailable,
    ];

    /// A second, independent spelling of the table. The match is exhaustive,
    /// so adding a variant fails to compile here until it is written down.
    fn expected_str(status: ResolverStatus) -> &'static str {
        match status {
            ResolverStatus::Ok => "ok",
            ResolverStatus::Degraded => "degraded",
            ResolverStatus::Unavailable => "unavailable",
        }
    }

    #[test]
    fn there_are_exactly_three_statuses() {
        assert_eq!(ALL.len(), 3);
    }

    #[test]
    fn as_str_and_parse_round_trip_for_every_status() {
        for status in ALL {
            assert_eq!(status.as_str(), expected_str(status));
            assert_eq!(
                ResolverStatus::parse(status.as_str()),
                Some(status),
                "{} does not round-trip",
                status.as_str()
            );
        }
    }

    #[test]
    fn parse_rejects_anything_else() {
        for value in ["", "OK", "Degraded", "unknown", "ok "] {
            assert_eq!(ResolverStatus::parse(value), None, "{value:?} was accepted");
        }
    }

    #[test]
    fn severity_orders_ok_before_degraded_before_unavailable() {
        assert!(ResolverStatus::Ok.severity() < ResolverStatus::Degraded.severity());
        assert!(ResolverStatus::Degraded.severity() < ResolverStatus::Unavailable.severity());
    }

    /// Worst wins, over every ordered pair.
    #[test]
    fn combine_takes_the_worst_of_every_pair() {
        for left in ALL {
            for right in ALL {
                let worst = if left.severity() >= right.severity() { left } else { right };
                assert_eq!(
                    ResolverStatus::combine(vec![left, right]),
                    worst,
                    "combine({}, {})",
                    left.as_str(),
                    right.as_str()
                );
            }
        }
    }

    #[test]
    fn combine_of_nothing_is_ok() {
        // An adapter that ran no resolver at all is not "degraded".
        assert_eq!(ResolverStatus::combine(Vec::new()), ResolverStatus::Ok);
        assert_eq!(
            ResolverStatus::combine(vec![ResolverStatus::Ok]),
            ResolverStatus::Ok
        );
        assert_eq!(
            ResolverStatus::combine(vec![
                ResolverStatus::Ok,
                ResolverStatus::Unavailable,
                ResolverStatus::Degraded,
            ]),
            ResolverStatus::Unavailable
        );
    }

    #[test]
    fn the_metadata_key_is_stable() {
        // graphlens reads this key out of a stored graph.
        assert_eq!(RESOLVER_STATUS_KEY, "resolver_status");
    }
}
