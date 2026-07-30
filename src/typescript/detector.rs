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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::visitor::TsImportClassifier;

    fn sorted(names: HashSet<String>) -> Vec<String> {
        let mut out: Vec<String> = names.into_iter().collect();
        out.sort();
        out
    }

    /// The project name becomes part of every qualified name, so it has to be
    /// an identifier: anything that is not `[a-z0-9_]` collapses to `_`.
    #[test]
    fn normalizes_a_package_name_into_identifier_shape() {
        assert_eq!(normalize_name("My-Pkg"), "my_pkg");
        assert_eq!(normalize_name("my.cool.pkg"), "my_cool_pkg");
        assert_eq!(normalize_name("already_fine"), "already_fine");
        assert_eq!(normalize_name("Web2"), "web2");
        // Leading and trailing separators are trimmed, inner ones are not.
        assert_eq!(normalize_name("_leading"), "leading");
        assert_eq!(normalize_name("trailing-"), "trailing");
    }

    /// A name made only of separators would normalize to nothing, and an
    /// empty project name would put every node under `.`; the raw name is
    /// kept instead.
    #[test]
    fn a_name_that_normalizes_to_nothing_is_kept_as_it_is() {
        assert_eq!(normalize_name("---"), "---");
        assert_eq!(normalize_name("__"), "__");
        assert_eq!(normalize_name(""), "");
    }

    #[test]
    fn the_marker_and_exclusion_lists_are_the_documented_ones() {
        assert_eq!(TYPESCRIPT_MARKERS, ["package.json", "tsconfig.json"]);
        // Node's own build output, on top of the shared service directories.
        for extra in ["out", "coverage", ".next", ".nuxt", "node_modules"] {
            assert!(TS_EXCLUDED_DIRS.contains(&extra), "{extra} is not excluded");
        }
    }

    #[test]
    fn node_builtins_are_listed_without_the_node_scheme() {
        let builtins = node_builtins();
        for name in ["fs", "path", "http", "worker_threads"] {
            assert!(builtins.contains(name), "{name} is missing");
        }
        assert!(!builtins.iter().any(|n| n.starts_with("node:")));
    }

    // --- module-specifier classification ---------------------------------
    //
    // The classifier lives in the visitor; the sets it works against are
    // built here, which is why the tests sit in this file.

    fn classifier() -> TsImportClassifier {
        TsImportClassifier::from_sets(
            node_builtins(),
            // Stored the way `dependencies()` stores them: normalized.
            sorted(
                ["react", "react-dom", "@scope/pkg"]
                    .iter()
                    .map(|n| crate::python::normalize_pkg_name(n))
                    .collect(),
            )
            .into_iter()
            .collect(),
            ["src"].iter().map(|n| (*n).to_string()).collect(),
        )
    }

    #[test]
    fn classifies_a_bare_specifier_by_its_first_segment() {
        let c = classifier();
        assert_eq!(c.classify("fs"), "stdlib");
        assert_eq!(c.classify("path/posix"), "stdlib");
        assert_eq!(c.classify("react"), "third_party");
        assert_eq!(c.classify("react/jsx-runtime"), "third_party");
        assert_eq!(c.classify("react-dom"), "third_party");
        assert_eq!(c.classify("src/app/main"), "internal");
        assert_eq!(c.classify("nowhere"), "unknown");
        assert_eq!(c.classify(""), "unknown");
    }

    /// A scoped package is two segments, not one: `@scope/pkg/sub` belongs to
    /// `@scope/pkg`, while `@scope/other` is a different package.
    #[test]
    fn a_scoped_package_key_is_two_segments() {
        let c = classifier();
        assert_eq!(c.classify("@scope/pkg"), "third_party");
        assert_eq!(c.classify("@scope/pkg/sub/deep"), "third_party");
        assert_eq!(c.classify("@scope/other"), "unknown");
        assert_eq!(c.classify("@scope"), "unknown");
    }

    /// A prefix that is a substring of a declared name is not a match.
    #[test]
    fn a_substring_of_a_dependency_is_not_that_dependency() {
        let c = classifier();
        assert_eq!(c.classify("reac"), "unknown");
        assert_eq!(c.classify("react-native"), "unknown");
    }

    /// KNOWN GAP: `node_builtins()` is documented as being "without the
    /// `node:` scheme", but nothing strips the scheme before the lookup, so
    /// `import fs from "node:fs"` — the form Node's own docs recommend —
    /// classifies as `unknown` instead of `stdlib`. Pinned as it stands; the
    /// fix belongs in the visitor's `package_key`.
    #[test]
    fn the_node_scheme_is_not_stripped() {
        assert_eq!(classifier().classify("node:fs"), "unknown");
    }
}
