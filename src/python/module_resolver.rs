//! A module's qualified name from its file path, and source-root detection.

use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

/// Python source roots: the `src/` layout first, otherwise the project root.
pub fn source_roots(project_root: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let src = project_root.join("src");
    if src.is_dir()
        && !files.is_empty()
        && files.iter().any(|f| f.starts_with(&src))
    {
        return vec![src, project_root.to_path_buf()];
    }
    vec![project_root.to_path_buf()]
}

/// File path → dotted module name.
///
/// `src/pkg/__init__.py` → `pkg`, `src/pkg/utils.py` → `pkg.utils`.
/// None if the file lies outside a source root.
pub fn qualified_name(file_path: &Path, source_root: &Path) -> Option<String> {
    let relative = file_path.strip_prefix(source_root).ok()?;
    let mut parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();

    let last = parts.last_mut()?;
    // Strip .py / .pyi
    if let Some(stem) = Path::new(last.as_str()).file_stem() {
        *last = stem.to_string_lossy().into_owned();
    }
    // For __init__ the module is the package itself
    if last == "__init__" {
        parts.pop();
    }

    if parts.is_empty() {
        // __init__.py at the root itself — take the root's name
        return source_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
    }
    Some(parts.join("."))
}

/// The first source root the file belongs to.
pub fn source_root_for<'a>(file: &Path, roots: &'a [PathBuf]) -> Option<&'a PathBuf> {
    roots.iter().find(|root| file.starts_with(root))
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn file_to_qualified_name(file_path: &str, source_root: &str) -> PyResult<String> {
    qualified_name(Path::new(file_path), Path::new(source_root)).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "{file_path:?} is not in the subpath of {source_root:?}"
        ))
    })
}

/// `__init__.py` or `__init__.pyi`.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn is_package_init(file_path: &str) -> bool {
    matches!(
        Path::new(file_path).file_name().map(|n| n.to_string_lossy()),
        Some(name) if name == "__init__.py" || name == "__init__.pyi"
    )
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn find_source_roots(project_root: &str, files: Vec<String>) -> Vec<String> {
    let files: Vec<PathBuf> = files.into_iter().map(PathBuf::from).collect();
    source_roots(Path::new(project_root), &files)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}
