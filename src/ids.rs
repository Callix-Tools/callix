//! Deterministic node IDs: the same input yields the same ID across runs,
//! which is what makes incremental updates and diffing possible.

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use sha2::{Digest, Sha256};

fn digest16(key: &str) -> String {
    let hash = Sha256::digest(key.as_bytes());
    hash[..8].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn node_id(project_name: &str, qualified_name: &str, kind: &str) -> String {
    digest16(&format!("{project_name}::{kind}::{qualified_name}"))
}

pub fn boundary_id(mechanism: &str, key: &str) -> String {
    digest16(&format!("boundary::{mechanism}::{key}"))
}

/// Stable node ID: sha256("{project}::{kind}::{qualified_name}")[:16].
#[gen_stub_pyfunction]
#[pyfunction]
pub fn make_node_id(project_name: &str, qualified_name: &str, kind: &str) -> String {
    node_id(project_name, qualified_name, kind)
}

/// Stable boundary ID — independent of both language and project, so a
/// server in one language and a client in another produce the same node.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn make_boundary_id(mechanism: &str, key: &str) -> String {
    boundary_id(mechanism, key)
}
