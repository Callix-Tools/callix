//! Finding YAML worth analysing.
//!
//! There is no manifest to look for — no `package.json` equivalent declares "a
//! YAML project". So a root is simply a directory containing YAML, and the
//! project name is the directory's. That is deliberately looser than the other
//! adapters, because the alternative would be to invent a convention nobody
//! follows.

use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::roots::any_file_with_extension;

pub const YAML_EXTENSIONS: [&str; 2] = [".yaml", ".yml"];

/// Whether any YAML lives under the root.
pub fn is_yaml(root: &Path) -> bool {
    any_file_with_extension(root, "yaml") || any_file_with_extension(root, "yml")
}

/// The single root: YAML has no nesting to discover.
///
/// Every other adapter looks for markers and can return several roots; here a
/// document belongs to whatever tree it sits in, so splitting would be
/// arbitrary.
pub fn yaml_roots(root: &Path) -> Vec<PathBuf> {
    if is_yaml(root) {
        vec![root.to_path_buf()]
    } else {
        Vec::new()
    }
}

pub fn project_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn is_yaml_project(root: PathBuf) -> bool {
    is_yaml(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn find_yaml_roots(root: PathBuf) -> Vec<PathBuf> {
    yaml_roots(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn yaml_detect_project_name(root: PathBuf) -> String {
    project_name(&root)
}
