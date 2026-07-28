//! Разбор go.mod и определение Go-проектов.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::roots::collect_marker_roots;

const GO_MARKERS: [&str; 1] = ["go.mod"];
const GO_EXCLUDED_DIRS: [&str; 4] = ["vendor", ".git", "node_modules", "testdata"];

/// Путь модуля из директивы `module` в go.mod.
pub fn module_path(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join("go.mod")).ok()?;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("module")
            && rest.starts_with([' ', '\t'])
        {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(value.split_whitespace().next()?.to_string());
            }
        }
    }
    None
}

/// Пути модулей из директив `require` — и блочных, и однострочных.
pub fn required_modules(root: &Path) -> HashSet<String> {
    let mut modules = HashSet::new();
    let Ok(text) = fs::read_to_string(root.join("go.mod")) else {
        return modules;
    };

    // Блок require ( ... )
    let mut rest = text.as_str();
    while let Some(start) = rest.find("require") {
        let after = &rest[start + "require".len()..];
        let Some(open) = after.find('(') else {
            rest = after;
            continue;
        };
        // Между `require` и `(` допустимы только пробелы.
        if !after[..open].chars().all(char::is_whitespace) {
            rest = after;
            continue;
        }
        let body = &after[open + 1..];
        let Some(close) = body.find(')') else {
            break;
        };
        for line in body[..close].lines() {
            let entry = line.trim();
            if entry.is_empty() || entry.starts_with("//") {
                continue;
            }
            if let Some(path) = entry.split_whitespace().next() {
                modules.insert(path.to_string());
            }
        }
        rest = &body[close..];
    }

    // Однострочный `require <path> <version>`: пробелы только внутри
    // строки, иначе открывающая скобка блока попала бы в путь.
    for line in text.lines() {
        let trimmed = line.trim_start_matches([' ', '\t']);
        let Some(after) = trimmed.strip_prefix("require") else {
            continue;
        };
        if !after.starts_with([' ', '\t']) {
            continue;
        }
        let mut parts = after.split_whitespace();
        if let (Some(path), Some(_version)) = (parts.next(), parts.next())
            && path != "("
        {
            modules.insert(path.to_string());
        }
    }
    modules
}

/// Есть ли в каталоге go.mod.
pub fn is_go(root: &Path) -> bool {
    root.join("go.mod").is_file()
}

/// Корни модулей Go при обходе монорепозитория.
pub fn go_roots(root: &Path) -> Vec<PathBuf> {
    let roots = collect_marker_roots(root, &GO_MARKERS, &GO_EXCLUDED_DIRS, |_| true);
    // Общая функция откатывается к самому корню; для Go этого не нужно.
    if roots.len() == 1 && roots[0] == root && !is_go(root) {
        return Vec::new();
    }
    roots
}

/// Имя проекта — последний сегмент пути модуля.
pub fn project_name(root: &Path) -> String {
    match module_path(root) {
        Some(path) => path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&path)
            .to_string(),
        None => root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn is_go_project(root: PathBuf) -> bool {
    is_go(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn find_go_roots(root: PathBuf) -> Vec<PathBuf> {
    go_roots(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn go_detect_project_name(root: PathBuf) -> String {
    project_name(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn go_read_module_path(root: PathBuf) -> Option<String> {
    module_path(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn go_parse_dependencies(root: PathBuf) -> HashSet<String> {
    required_modules(&root)
}

/// Классификация пути импорта Go.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (import_path, module_path = None, required = None))]
pub fn classify_go_import(
    import_path: &str,
    module_path: Option<&str>,
    required: Option<HashSet<String>>,
) -> &'static str {
    super::visitor::classify_import(import_path, module_path, &required.unwrap_or_default())
}
