//! Разбор манифестов: откуда берутся имена сторонних пакетов.
//!
//! Dev/test-группы включены намеренно — иначе импорты из тестов
//! классифицировались бы как `unknown`, а не `third_party`.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::roots::ini_sections;

/// Приводит имя дистрибутива к виду, сравнимому с именем импорта.
///
/// Отрезает версии и extras (`requests>=2.0 [security]` → `requests`),
/// переводит в нижний регистр, дефисы меняет на подчёркивания.
/// Scoped-имена npm (`@scope/pkg`) остаются как есть.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn normalize_pkg_name(name: &str) -> String {
    let mut name = name.split('#').next().unwrap_or("").trim();
    for sep in ['[', '>', '<', '=', '!', '~', ';', ' '] {
        name = name.split(sep).next().unwrap_or("");
    }
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    if name.starts_with('@') {
        return name.to_lowercase();
    }
    name.to_lowercase().replace('-', "_")
}

fn add(names: &mut HashSet<String>, raw: &str) {
    let normalized = normalize_pkg_name(raw);
    if !normalized.is_empty() {
        names.insert(normalized);
    }
}

/// PEP 621 (`[project]`) и Poetry (`[tool.poetry]`) из pyproject.toml.
fn parse_pyproject(root: &Path, names: &mut HashSet<String>) {
    let path = root.join("pyproject.toml");
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return;
    };

    if let Some(project) = table.get("project").and_then(|v| v.as_table()) {
        if let Some(deps) = project.get("dependencies").and_then(|v| v.as_array()) {
            for dep in deps.iter().filter_map(|d| d.as_str()) {
                add(names, dep);
            }
        }
        if let Some(groups) = project
            .get("optional-dependencies")
            .and_then(|v| v.as_table())
        {
            for group in groups.values().filter_map(|g| g.as_array()) {
                for dep in group.iter().filter_map(|d| d.as_str()) {
                    add(names, dep);
                }
            }
        }
    }

    let Some(poetry) = table
        .get("tool")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("poetry"))
        .and_then(|v| v.as_table())
    else {
        return;
    };
    if let Some(deps) = poetry.get("dependencies").and_then(|v| v.as_table()) {
        for dep in deps.keys() {
            let normalized = normalize_pkg_name(dep);
            if !normalized.is_empty() && normalized != "python" {
                names.insert(normalized);
            }
        }
    }
    if let Some(deps) = poetry.get("dev-dependencies").and_then(|v| v.as_table()) {
        for dep in deps.keys() {
            add(names, dep);
        }
    }
    // Группы зависимостей poetry >= 1.2
    if let Some(groups) = poetry.get("group").and_then(|v| v.as_table()) {
        for group in groups.values().filter_map(|g| g.as_table()) {
            if let Some(deps) = group.get("dependencies").and_then(|v| v.as_table()) {
                for dep in deps.keys() {
                    add(names, dep);
                }
            }
        }
    }
}

/// Строки URL/VCS пропускаются: имя импорта из них не вытащить.
fn is_skipped(line: &str) -> bool {
    let line = line.trim_start();
    ["-r", "-c", "-e", "https://", "http://", "git+", "svn+", "hg+", "bzr+"]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

fn parse_requirements_file(path: &Path, root: &Path, names: &mut HashSet<String>, depth: u8) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if is_skipped(line) {
            // Включения `-r other.txt` разворачиваем на один уровень.
            if let Some(reference) = line.strip_prefix("-r")
                && depth == 0
            {
                let referenced = root.join(reference.trim());
                if referenced.exists() {
                    parse_requirements_file(&referenced, root, names, depth + 1);
                }
            }
            continue;
        }
        add(names, line);
    }
}

/// Файлы `requirements*.txt` в корне проекта.
fn parse_requirements(root: &Path, names: &mut HashSet<String>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("requirements") && n.ends_with(".txt"))
        })
        .collect();
    files.sort();
    for file in files {
        parse_requirements_file(&file, root, names, 0);
    }
}

/// `install_requires` из `[options]` и `[options.extras_require*]`.
fn parse_setup_cfg(root: &Path, names: &mut HashSet<String>) {
    let path = root.join("setup.cfg");
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    for (section, values) in ini_sections(&text, "install_requires") {
        if section == "options" || section.starts_with("options.extras_require") {
            for line in values {
                add(names, &line);
            }
        }
    }
}

/// Имена сторонних пакетов, объявленных в манифестах корня.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn parse_dependencies(project_root: &str) -> HashSet<String> {
    let root = Path::new(project_root);
    let mut names = HashSet::new();
    parse_pyproject(root, &mut names);
    parse_requirements(root, &mut names);
    parse_setup_cfg(root, &mut names);
    names
}

/// Имена модулей stdlib верхнего уровня для текущего интерпретатора.
///
/// Берётся из `sys.stdlib_module_names` — это свойство запущенного
/// Python, а не сборки, поэтому считается на стороне интерпретатора.
pub fn stdlib_names(py: Python<'_>) -> PyResult<HashSet<String>> {
    py.import("sys")?
        .getattr("stdlib_module_names")?
        .extract::<HashSet<String>>()
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn get_stdlib_names(py: Python<'_>) -> PyResult<HashSet<String>> {
    stdlib_names(py)
}
