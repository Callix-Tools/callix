//! Детерминированные ID узлов: одинаковые входные данные — одинаковый ID
//! между запусками, что и делает возможными инкрементальные обновления и diff.

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

/// Стабильный ID узла: sha256("{project}::{kind}::{qualified_name}")[:16].
#[gen_stub_pyfunction]
#[pyfunction]
pub fn make_node_id(project_name: &str, qualified_name: &str, kind: &str) -> String {
    node_id(project_name, qualified_name, kind)
}

/// Стабильный ID границы — не зависит ни от языка, ни от проекта, поэтому
/// сервер на одном языке и клиент на другом дают один и тот же узел.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn make_boundary_id(mechanism: &str, key: &str) -> String {
    boundary_id(mechanism, key)
}
