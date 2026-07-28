//! Detecting TypeScript projects and parsing package.json.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::roots::{any_file_with_extension, collect_marker_roots};

pub const TYPESCRIPT_MARKERS: [&str; 2] = ["package.json", "tsconfig.json"];

/// On top of the shared service directories, Node has build artifacts of its
/// own.
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

/// Normalizes a name into identifier shape: lowercase, everything else `_`.
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

/// Whether there is TypeScript under the root: markers first, then any
/// `.ts`/`.tsx`.
pub fn is_typescript(project_root: &Path) -> bool {
    has_markers(project_root)
        || any_file_with_extension(project_root, "ts")
        || any_file_with_extension(project_root, "tsx")
}

/// TypeScript project roots (monorepo-aware).
///
/// A tsconfig inside an already-found package.json root counts as that
/// package's config rather than as a root of its own.
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

/// `collect_marker_roots` without the fallback to `search_root`.
fn collect_marker_roots_no_fallback(
    search_root: &Path,
    markers: &[&str],
    excluded: &[&str],
) -> Vec<PathBuf> {
    let roots = collect_marker_roots(search_root, markers, excluded, |_| true);
    // The shared helper falls back to the root itself; not wanted here.
    if roots.len() == 1 && roots[0] == search_root && !markers.iter().any(|m| search_root.join(m).exists())
    {
        return Vec::new();
    }
    roots
}

/// The project name: `name` from package.json (npm scope stripped),
/// otherwise the directory name.
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

/// Dependencies from package.json.
///
/// `dependencies`, `devDependencies`, `peerDependencies`, and
/// `optionalDependencies` are all taken, so imports from tests and peer
/// dependencies classify as `third_party` rather than `unknown`.
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

/// Node.js built-in module names — without the `node:` scheme.
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
