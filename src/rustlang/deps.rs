//! Разбор Cargo.toml, поиск крейтов и классификация путей `use`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::roots::collect_marker_roots;

const RUST_MARKERS: [&str; 1] = ["Cargo.toml"];
const RUST_EXCLUDED_DIRS: [&str; 3] = ["target", ".git", "node_modules"];

const DEP_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
/// Крейты стандартной поставки Rust.
const STDLIB_CRATES: [&str; 5] = ["std", "core", "alloc", "proc_macro", "test"];
/// Корни пути, всегда указывающие внутрь текущего крейта.
const INTERNAL_ROOTS: [&str; 3] = ["crate", "self", "super"];

fn load_cargo(root: &Path) -> Option<toml::Table> {
    let text = fs::read_to_string(root.join("Cargo.toml")).ok()?;
    text.parse::<toml::Table>().ok()
}

/// Имя крейта из `[package].name`.
pub fn crate_name(root: &Path) -> Option<String> {
    let data = load_cargo(root)?;
    let name = data.get("package")?.as_table()?.get("name")?.as_str()?;
    Some(name.to_string())
}

/// Имена крейтов из всех таблиц зависимостей.
///
/// Дефисы заменяются на подчёркивания: в `Cargo.toml` крейт зовётся
/// `tree-sitter`, а в коде — `tree_sitter`.
pub fn dependencies(root: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let Some(data) = load_cargo(root) else {
        return names;
    };
    for table in DEP_TABLES {
        if let Some(section) = data.get(table).and_then(toml::Value::as_table) {
            names.extend(section.keys().map(|k| k.replace('-', "_")));
        }
    }
    names
}

/// Есть ли в каталоге Cargo.toml.
pub fn is_rust(root: &Path) -> bool {
    root.join("Cargo.toml").is_file()
}

/// Корни крейтов под `root`; Cargo.toml в самом корне не скрывает вложенные.
pub fn rust_roots(root: &Path) -> Vec<PathBuf> {
    let roots = collect_marker_roots(root, &RUST_MARKERS, &RUST_EXCLUDED_DIRS, |_| true);
    // Общая функция откатывается к самому корню; для Rust этого не нужно.
    if roots.len() == 1 && roots[0] == root && !is_rust(root) {
        return Vec::new();
    }
    roots
}

/// Имя проекта — имя крейта, иначе имя каталога.
pub fn project_name(root: &Path) -> String {
    crate_name(root).unwrap_or_else(|| {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// Классификация пути `use`.
///
/// `crate`/`self`/`super` и имя самого крейта — внутреннее; `std`/`core`/
/// `alloc` — стандартная библиотека; перечисленное в таблицах зависимостей —
/// стороннее. Остальное (например, транзитивная зависимость) — `unknown`.
pub fn classify_import(
    import_path: &str,
    crate_name: Option<&str>,
    deps: &HashSet<String>,
) -> &'static str {
    let first = import_path.split("::").next().unwrap_or(import_path).trim();
    if STDLIB_CRATES.contains(&first) {
        return "stdlib";
    }
    if INTERNAL_ROOTS.contains(&first) {
        return "internal";
    }
    let normalized = first.replace('-', "_");
    if let Some(name) = crate_name
        && normalized == name.replace('-', "_")
    {
        return "internal";
    }
    if deps.contains(&normalized) {
        return "third_party";
    }
    "unknown"
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn is_rust_project(root: PathBuf) -> bool {
    is_rust(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn find_rust_roots(root: PathBuf) -> Vec<PathBuf> {
    rust_roots(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn rust_detect_project_name(root: PathBuf) -> String {
    project_name(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn rust_read_crate_name(root: PathBuf) -> Option<String> {
    crate_name(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn rust_parse_dependencies(root: PathBuf) -> HashSet<String> {
    dependencies(&root)
}

/// Классификация пути `use` — точка входа для проверок.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (import_path, crate_name = None, deps = None))]
pub fn classify_rust_import(
    import_path: &str,
    crate_name: Option<&str>,
    deps: Option<HashSet<String>>,
) -> &'static str {
    classify_import(import_path, crate_name, &deps.unwrap_or_default())
}
