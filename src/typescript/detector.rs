//! Определение TypeScript-проектов и разбор package.json.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::roots::{any_file_with_extension, collect_marker_roots};

pub const TYPESCRIPT_MARKERS: [&str; 2] = ["package.json", "tsconfig.json"];

/// Помимо общих служебных каталогов, у Node свои артефакты сборки.
pub const TS_EXCLUDED_DIRS: [&str; 12] = [
    ".venv",
    "venv",
    "__pycache__",
    ".git",
    "dist",
    "build",
    ".eggs",
    "node_modules",
    "out",
    "coverage",
    ".next",
    ".nuxt",
];

/// Приводит имя к виду идентификатора: нижний регистр, всё лишнее — в `_`.
fn normalize_name(name: &str) -> String {
    let normalized: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = normalized.trim_matches('_').to_string();
    if trimmed.is_empty() {
        name.to_string()
    } else {
        trimmed
    }
}

fn has_markers(directory: &Path) -> bool {
    TYPESCRIPT_MARKERS
        .iter()
        .any(|marker| directory.join(marker).exists())
}

/// Есть ли под корнем TypeScript: сначала маркеры, потом любой `.ts`/`.tsx`.
pub fn is_typescript(project_root: &Path) -> bool {
    has_markers(project_root)
        || any_file_with_extension(project_root, "ts")
        || any_file_with_extension(project_root, "tsx")
}

/// Корни TypeScript-проектов (поддержка монорепозиториев).
///
/// tsconfig внутри уже найденного package.json-корня считается конфигом
/// этого пакета, а не отдельным корнем.
pub fn typescript_roots(search_root: &Path) -> Vec<PathBuf> {
    let package_roots =
        collect_marker_roots_no_fallback(search_root, &["package.json"], &TS_EXCLUDED_DIRS);
    let tsconfig_roots =
        collect_marker_roots_no_fallback(search_root, &["tsconfig.json"], &TS_EXCLUDED_DIRS);

    let mut roots = package_roots.clone();
    for tsconfig_root in tsconfig_roots {
        if roots.contains(&tsconfig_root) {
            continue;
        }
        if package_roots
            .iter()
            .any(|root| tsconfig_root.starts_with(root))
        {
            continue;
        }
        roots.push(tsconfig_root);
    }

    if roots.is_empty() {
        return vec![search_root.to_path_buf()];
    }
    crate::roots::sort_paths_like_python(&mut roots);
    roots
}

/// `collect_marker_roots` без отката к `search_root`.
fn collect_marker_roots_no_fallback(
    search_root: &Path,
    markers: &[&str],
    excluded: &[&str],
) -> Vec<PathBuf> {
    let roots = collect_marker_roots(search_root, markers, excluded, |_| true);
    // Общая функция откатывается к самому корню; здесь это не нужно.
    if roots.len() == 1 && roots[0] == search_root && !markers.iter().any(|m| search_root.join(m).exists())
    {
        return Vec::new();
    }
    roots
}

/// Имя проекта: `name` из package.json (без npm-скоупа), иначе имя каталога.
pub fn project_name(project_root: &Path) -> String {
    let package_json = project_root.join("package.json");
    if package_json.exists()
        && let Ok(raw) = fs::read_to_string(&package_json)
        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(raw_name) = data.get("name").and_then(|n| n.as_str())
        && !raw_name.is_empty()
    {
        // "@scope/pkg" → "pkg"
        let raw_name = match raw_name.split_once('/') {
            Some((scope, rest)) if scope.starts_with('@') => rest,
            _ => raw_name,
        };
        let name = normalize_name(raw_name);
        if !name.is_empty() {
            return name;
        }
    }
    normalize_name(
        &project_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    )
}

/// Зависимости из package.json.
///
/// Берутся `dependencies`, `devDependencies`, `peerDependencies` и
/// `optionalDependencies`, чтобы импорты из тестов и peer-зависимости
/// классифицировались как `third_party`, а не `unknown`.
pub fn dependencies(project_root: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(raw) = fs::read_to_string(project_root.join("package.json")) else {
        return names;
    };
    let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return names;
    };
    for section in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        if let Some(table) = data.get(section).and_then(|s| s.as_object()) {
            for dep in table.keys() {
                let normalized = crate::python::normalize_pkg_name(dep);
                if !normalized.is_empty() {
                    names.insert(normalized);
                }
            }
        }
    }
    names
}

/// Имена встроенных модулей Node.js — без схемы `node:`.
pub fn node_builtins() -> HashSet<String> {
    [
        "assert", "async_hooks", "buffer", "child_process", "cluster", "console", "constants",
        "crypto", "dgram", "diagnostics_channel", "dns", "domain", "events", "fs", "http",
        "http2", "https", "inspector", "module", "net", "os", "path", "perf_hooks", "process",
        "punycode", "querystring", "readline", "repl", "stream", "string_decoder", "sys",
        "timers", "tls", "trace_events", "tty", "url", "util", "v8", "vm", "wasi",
        "worker_threads", "zlib",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn is_typescript_project(project_root: PathBuf) -> bool {
    is_typescript(&project_root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn find_typescript_roots(search_root: PathBuf) -> Vec<PathBuf> {
    typescript_roots(&search_root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn ts_detect_project_name(project_root: PathBuf) -> String {
    project_name(&project_root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn ts_parse_dependencies(project_root: PathBuf) -> HashSet<String> {
    dependencies(&project_root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn ts_get_stdlib_names() -> HashSet<String> {
    node_builtins()
}
