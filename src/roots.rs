//! Finding language-project roots in a monorepo and collecting sources.
//!
//! The general rule: a marker in `search_root` itself does NOT hide nested
//! roots — otherwise a monorepo that is a project in its own right would
//! swallow its subprojects.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// Directories that are not sources for any language.
pub const EXCLUDED_DIRS: [&str; 8] = [
    ".venv",
    "venv",
    "__pycache__",
    ".git",
    "dist",
    "build",
    ".eggs",
    "node_modules",
];

/// Whether the path lies inside an excluded directory (relative to the root).
fn in_excluded_dir(path: &Path, search_root: &Path, excluded: &[&str]) -> bool {
    let Ok(relative) = path.strip_prefix(search_root) else {
        return false;
    };
    relative
        .components()
        .any(|c| excluded.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

/// Whether the path's FIRST component is excluded.
///
/// `vendor` and `target` are written at a project's root by its package
/// manager and nowhere else, so they are matched there rather than at any
/// depth: a crate may legitimately hold `src/target/mod.rs`, and a Go package
/// may be named `vendor` deeper in the tree. Anchoring drops the third-party
/// tree without hiding first-party code that happens to share the name.
fn under_excluded_root_dir(path: &Path, search_root: &Path, excluded: &[&str]) -> bool {
    if excluded.is_empty() {
        return false;
    }
    let Ok(relative) = path.strip_prefix(search_root) else {
        return false;
    };
    relative
        .components()
        .next()
        .is_some_and(|c| excluded.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

/// Path sorting the way Python's `sorted()` over `pathlib.Path` works.
///
/// Python compares paths COMPONENT BY COMPONENT rather than as whole
/// strings, and neighbours like `src/v4-mini/...` and `src/v4/...` diverge
/// on exactly that: as strings `v4-mini` comes first (hyphen < slash), by
/// component `v4` does (the shorter component is a prefix of the longer).
fn path_sort_key(path: &Path) -> Vec<String> {
    path.to_string_lossy()
        .split('/')
        .map(str::to_string)
        .collect()
}

pub fn sort_paths_like_python(paths: &mut [PathBuf]) {
    sort_paths(paths);
}

fn sort_paths(paths: &mut [PathBuf]) {
    paths.sort_by_key(|p| path_sort_key(p));
}

pub fn is_relative_to(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root).is_ok()
}

/// Every directory holding a valid project marker.
///
/// `marker_filter` rejects false positives (for instance a pyproject.toml
/// with no `[project]` section). An empty result falls back to `search_root`
/// itself.
pub fn collect_marker_roots(
    search_root: &Path,
    markers: &[&str],
    excluded: &[&str],
    marker_filter: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for entry in WalkDir::new(search_root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !markers.contains(&name.as_ref()) {
            continue;
        }
        if in_excluded_dir(path, search_root, excluded) || !marker_filter(path) {
            continue;
        }
        let Some(root) = path.parent() else { continue };
        if seen.insert(root.to_path_buf()) {
            roots.push(root.to_path_buf());
        }
    }

    if roots.is_empty() {
        return vec![search_root.to_path_buf()];
    }
    sort_paths(&mut roots);
    roots
}

/// Drops files belonging to nested roots: a parent root must not parse its
/// subprojects' files.
pub fn filter_nested_root_files(
    files: Vec<PathBuf>,
    current_root: &Path,
    project_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let nested: Vec<&PathBuf> = project_roots
        .iter()
        .filter(|root| root.as_path() != current_root && is_relative_to(root, current_root))
        .collect();

    files
        .into_iter()
        .filter(|file| !nested.iter().any(|root| is_relative_to(file, root)))
        .collect()
}

/// Every file with the given extensions under the root, excluding service
/// directories.
///
/// `excluded` matches a directory name at any depth; `excluded_at_root`
/// matches only the root's own children, which is where a package manager
/// puts a third-party tree. Passing the latter is what keeps `vendor/` and
/// `target/` out of the graph — without it a vendored dependency is reported
/// as first-party code under a fabricated qualified name.
pub fn collect_files(
    root: &Path,
    extensions: &[&str],
    excluded: &[&str],
    excluded_at_root: &[&str],
) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            let matches_ext = p
                .extension()
                .map(|e| {
                    let dotted = format!(".{}", e.to_string_lossy());
                    extensions.contains(&dotted.as_str())
                })
                .unwrap_or(false);
            matches_ext
                && !in_excluded_dir(p, root, excluded)
                && !under_excluded_root_dir(p, root, excluded_at_root)
        })
        .collect();
    sort_paths(&mut files);
    files
}

/// Whether at least one file with this extension exists under the root.
pub fn any_file_with_extension(root: &Path, extension: &str) -> bool {
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .any(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|ext| ext == extension)
        })
}

/// The key's values, per section of an INI file.
///
/// Understands indented continuation lines — which is how `install_requires`
/// in setup.cfg is almost always written:
///
/// ```ini
/// [options]
/// install_requires =
///     requests
///     httpx
/// ```
///
/// Keys are compared case-insensitively (like configparser's `optionxform`),
/// section names case-sensitively.
pub fn ini_sections(text: &str, key: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut section = String::new();
    let mut collecting: Option<Vec<String>> = None;

    let flush = |out: &mut Vec<(String, Vec<String>)>,
                 section: &str,
                 collected: Option<Vec<String>>| {
        if let Some(values) = collected {
            out.push((section.to_string(), values));
        }
    };

    for raw in text.lines() {
        let trimmed = raw.trim();
        let indented = raw.starts_with([' ', '\t']);

        // A value continues on indented lines until a new entry starts.
        if let Some(values) = collecting.as_mut()
            && indented
            && !trimmed.is_empty()
            && !trimmed.starts_with('[')
        {
            if !trimmed.starts_with('#') && !trimmed.starts_with(';') {
                values.push(trimmed.to_string());
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        if let Some(header) = trimmed.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            flush(&mut out, &section, collecting.take());
            section = header.trim().to_string();
            continue;
        }

        if let Some((name, value)) = trimmed.split_once('=') {
            flush(&mut out, &section, collecting.take());
            if name.trim().eq_ignore_ascii_case(key) {
                let value = value.trim();
                collecting = Some(if value.is_empty() {
                    Vec::new()
                } else {
                    vec![value.to_string()]
                });
            }
        }
    }
    flush(&mut out, &section, collecting);
    out
}
