//! Detecting Python projects: marker files and the project name.

use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::roots::{EXCLUDED_DIRS, any_file_with_extension, collect_marker_roots, ini_sections};

pub const PYTHON_MARKERS: [&str; 5] = [
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "Pipfile",
    "requirements.txt",
];

/// A pyproject.toml only counts with a `[project]` section — otherwise a
/// Rust project keeping a pyproject.toml for tooling would masquerade as a
/// Python project.
fn is_valid_marker(path: &Path) -> bool {
    if path.file_name().is_some_and(|n| n == "pyproject.toml") {
        return pyproject_has_project_section(path);
    }
    true
}

fn pyproject_has_project_section(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return fallback_has_py_files(path);
    };
    match text.parse::<toml::Table>() {
        Ok(table) => table.contains_key("project"),
        // Unparseable — fall back to whether any .py exists nearby.
        Err(_) => fallback_has_py_files(path),
    }
}

fn fallback_has_py_files(path: &Path) -> bool {
    path.parent()
        .is_some_and(|parent| any_file_with_extension(parent, "py"))
}

fn has_python_markers(directory: &Path) -> bool {
    PYTHON_MARKERS.iter().any(|marker| {
        let path = directory.join(marker);
        path.exists() && is_valid_marker(&path)
    })
}

pub fn python_roots(search_root: &Path) -> Vec<PathBuf> {
    collect_marker_roots(search_root, &PYTHON_MARKERS, &EXCLUDED_DIRS, is_valid_marker)
}

/// The project name: `[project].name` from pyproject.toml, then
/// `[metadata] name` from setup.cfg, then the directory name.
pub fn project_name(project_root: &Path) -> String {
    let pyproject = project_root.join("pyproject.toml");
    if pyproject.exists()
        && let Ok(text) = fs::read_to_string(&pyproject)
        && let Ok(table) = text.parse::<toml::Table>()
        && let Some(name) = table
            .get("project")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
        && !name.is_empty()
    {
        return name.to_string();
    }

    let setup_cfg = project_root.join("setup.cfg");
    if setup_cfg.exists()
        && let Ok(text) = fs::read_to_string(&setup_cfg)
        && let Some(name) = ini_first(&text, "metadata", "name")
    {
        return name;
    }

    project_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The first value of a key in an INI section.
fn ini_first(text: &str, section: &str, key: &str) -> Option<String> {
    ini_sections(text, key)
        .into_iter()
        .find(|(name, values)| name == section && !values.is_empty())
        .map(|(_, values)| values[0].clone())
}

/// Whether there is Python under the root: markers first, then any `.py`.
///
/// The `.py` fallback exists for polyglot monorepos whose root carries no
/// Python markers but holds Python subpackages next to JS/Rust.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn is_python_project(project_root: &str) -> bool {
    let root = Path::new(project_root);
    has_python_markers(root) || any_file_with_extension(root, "py")
}

/// Python project roots inside `search_root` (one per subproject).
#[gen_stub_pyfunction]
#[pyfunction]
pub fn find_python_roots(search_root: &str) -> Vec<String> {
    python_roots(Path::new(search_root))
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn detect_project_name(project_root: &str) -> String {
    project_name(Path::new(project_root))
}
