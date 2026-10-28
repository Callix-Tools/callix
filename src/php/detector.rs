//! Detecting PHP projects and reading composer.json.
//!
//! `composer.json` is the marker, but a great deal of PHP ships without one,
//! so detection falls back to "any `.php` file exists" the way graphlens did.
//! The PSR-4 map read here is what lets the visitor turn a namespace into a
//! directory and back.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::py::prelude::*;
use crate::py::derive::gen_stub_pyfunction;
use serde_json::Value;

use crate::roots::{any_file_with_extension, collect_marker_roots};

pub const PHP_EXTENSIONS: [&str; 1] = [".php"];

const PHP_MARKERS: [&str; 1] = ["composer.json"];

/// Service directories, matched at any depth.
///
/// `var` and `cache` are Symfony's writable trees, `.phpunit.cache` is a test
/// runner's. None of them hold code the project wrote.
pub const PHP_EXCLUDED_DIRS: [&str; 7] = [
    "node_modules",
    ".git",
    "var",
    "cache",
    "build",
    "dist",
    ".phpunit.cache",
];

/// Composer writes the third-party tree at the project root, and it is
/// enormous — a mid-sized Symfony application vendors more PHP than it
/// contains.
///
/// The list is applied twice: to file collection, relative to each sub-root
/// (`collect_php_files`), and to root discovery (`php_roots`), at the search
/// root and directly under any directory holding a `composer.json`. Installed
/// packages are therefore never sub-roots of their own.
pub const PHP_EXCLUDED_AT_ROOT: [&str; 1] = ["vendor"];

/// composer.json as an object, or empty when absent or malformed.
pub fn composer(root: &Path) -> BTreeMap<String, Value> {
    let Ok(text) = fs::read_to_string(root.join("composer.json")) else {
        return BTreeMap::new();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return BTreeMap::new();
    };
    map.into_iter().collect()
}

/// The PSR-4 prefix → directory map, longest prefix first.
///
/// Composer allows a list of directories per prefix; only the first is kept,
/// because a namespace has to map to one place for a qualified name to be
/// deterministic. Sorting by descending prefix length makes lookup a matter of
/// taking the first match: `App\Domain\` must win over `App\`.
pub fn psr4_roots(root: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let composer = composer(root);

    for section in ["autoload", "autoload-dev"] {
        let Some(Value::Object(autoload)) = composer.get(section) else {
            continue;
        };
        for key in ["psr-4", "psr-0"] {
            let Some(Value::Object(map)) = autoload.get(key) else {
                continue;
            };
            for (prefix, target) in map {
                let dir = match target {
                    Value::String(dir) => dir.clone(),
                    Value::Array(dirs) => match dirs.first() {
                        Some(Value::String(dir)) => dir.clone(),
                        _ => continue,
                    },
                    _ => continue,
                };
                out.push((prefix.clone(), dir.trim_end_matches('/').to_string()));
            }
        }
    }

    out.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// Whether the directory holds a PHP project.
pub fn is_php(root: &Path) -> bool {
    root.join("composer.json").is_file() || any_file_with_extension(root, "php")
}

/// Whether a `composer.json` belongs to an installed package, i.e. sits below
/// a `vendor` directory that is either the first component of its path
/// relative to `search_root`, or directly inside a directory that holds a
/// `composer.json` (that Composer project's own vendor tree).
fn in_composer_vendor(marker: &Path, search_root: &Path) -> bool {
    let Ok(rel) = marker.strip_prefix(search_root) else {
        return false;
    };
    let Some(parent) = rel.parent() else {
        return false;
    };
    let comps: Vec<_> = parent.components().collect();
    for (i, comp) in comps.iter().enumerate() {
        let name = comp.as_os_str().to_string_lossy();
        if !PHP_EXCLUDED_AT_ROOT.contains(&name.as_ref()) {
            continue;
        }
        if i == 0 {
            return true;
        }
        let mut dir = search_root.to_path_buf();
        for c in &comps[..i] {
            dir.push(c.as_os_str());
        }
        if dir.join("composer.json").is_file() {
            return true;
        }
    }
    false
}

/// PHP project roots when walking a monorepo.
///
/// A `composer.json` inside an installed package (under `vendor/`) is not a
/// root, so the result does not depend on whether `composer install` was run.
pub fn php_roots(root: &Path) -> Vec<PathBuf> {
    collect_marker_roots(root, &PHP_MARKERS, &PHP_EXCLUDED_DIRS, |marker| {
        !in_composer_vendor(marker, root)
    })
}

/// The project name — composer's `name`, else the directory.
pub fn project_name(root: &Path) -> String {
    match composer(root).get("name") {
        Some(Value::String(name)) if !name.is_empty() => name.clone(),
        _ => root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn is_php_project(root: PathBuf) -> bool {
    is_php(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn find_php_roots(root: PathBuf) -> Vec<PathBuf> {
    php_roots(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn php_detect_project_name(root: PathBuf) -> String {
    project_name(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(tag: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("callix-php-roots-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for f in files {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, if f.ends_with(".json") { "{}" } else { "<?php\n" }).unwrap();
        }
        root
    }

    #[test]
    fn php_roots_skips_installed_vendor_packages() {
        let root = tree(
            "a",
            &[
                "composer.json",
                "vendor/acme/lib/composer.json",
                "packages/x/composer.json",
                "packages/x/vendor/foo/bar/composer.json",
                "lib/vendor/y/composer.json",
            ],
        );
        assert_eq!(
            php_roots(&root),
            vec![root.clone(), root.join("lib/vendor/y"), root.join("packages/x")]
        );
        let pkg = root.join("vendor/acme/lib");
        assert_eq!(php_roots(&pkg), vec![pkg.clone()]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn php_roots_falls_back_to_the_search_root_when_only_vendor_has_markers() {
        let root = tree("b", &["vendor/acme/lib/composer.json", "src/a.php"]);
        assert_eq!(php_roots(&root), vec![root.clone()]);
        let _ = fs::remove_dir_all(&root);
    }
}
