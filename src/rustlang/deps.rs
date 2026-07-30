//! Parsing Cargo.toml, finding crates, and classifying `use` paths.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::roots::collect_marker_roots;

const RUST_MARKERS: [&str; 1] = ["Cargo.toml"];
const RUST_EXCLUDED_DIRS: [&str; 3] = ["target", ".git", "node_modules"];

const DEP_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
/// Crates from the Rust standard distribution.
const STDLIB_CRATES: [&str; 5] = ["std", "core", "alloc", "proc_macro", "test"];
/// Path roots that always point inside the current crate.
const INTERNAL_ROOTS: [&str; 3] = ["crate", "self", "super"];

fn load_cargo(root: &Path) -> Option<toml::Table> {
    let text = fs::read_to_string(root.join("Cargo.toml")).ok()?;
    text.parse::<toml::Table>().ok()
}

/// The crate name from `[package].name`.
pub fn crate_name(root: &Path) -> Option<String> {
    crate_name_in(&load_cargo(root)?)
}

/// The lookup half of [`crate_name`], split from the read so a manifest can
/// be handed in as text.
fn crate_name_in(data: &toml::Table) -> Option<String> {
    let name = data.get("package")?.as_table()?.get("name")?.as_str()?;
    Some(name.to_string())
}

/// Crate names from every dependency table.
///
/// Hyphens become underscores: in `Cargo.toml` a crate is called
/// `tree-sitter`, in code `tree_sitter`.
pub fn dependencies(root: &Path) -> HashSet<String> {
    match load_cargo(root) {
        Some(data) => dependencies_in(&data),
        None => HashSet::new(),
    }
}

/// The lookup half of [`dependencies`], split for the same reason as
/// [`crate_name_in`].
fn dependencies_in(data: &toml::Table) -> HashSet<String> {
    let mut names = HashSet::new();
    for table in DEP_TABLES {
        if let Some(section) = data.get(table).and_then(toml::Value::as_table) {
            names.extend(section.keys().map(|k| k.replace('-', "_")));
        }
    }
    names
}

/// Whether the directory holds a Cargo.toml.
pub fn is_rust(root: &Path) -> bool {
    root.join("Cargo.toml").is_file()
}

/// Crate roots under `root`; a Cargo.toml at the root does not hide nested
/// ones.
pub fn rust_roots(root: &Path) -> Vec<PathBuf> {
    let roots = collect_marker_roots(root, &RUST_MARKERS, &RUST_EXCLUDED_DIRS, |_| true);
    // The shared helper falls back to the root itself; Rust does not want
    // that.
    if roots.len() == 1 && roots[0] == root && !is_rust(root) {
        return Vec::new();
    }
    roots
}

/// The project name — the crate name, otherwise the directory name.
pub fn project_name(root: &Path) -> String {
    crate_name(root).unwrap_or_else(|| {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// Classification of a `use` path.
///
/// `crate`/`self`/`super` and the crate's own name are internal; `std`/`core`/
/// `alloc` are the standard library; anything listed in the dependency tables
/// is third-party. The rest (a transitive dependency, say) is `unknown`.
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

/// Classification of a `use` path — the entry point for checks.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(text: &str) -> toml::Table {
        text.parse::<toml::Table>().expect("the test manifest parses")
    }

    fn sorted(names: HashSet<String>) -> Vec<String> {
        let mut out: Vec<String> = names.into_iter().collect();
        out.sort();
        out
    }

    const FULL: &str = r#"
[package]
name = "my-crate"
version = "0.1.0"

[dependencies]
tree-sitter = "0.26"
serde = { version = "1", features = ["derive"] }

[dev-dependencies]
tempfile = "3"

[build-dependencies]
cc = "1"
"#;

    #[test]
    fn reads_the_package_name_verbatim() {
        // The hyphen is NOT normalized here: this is the manifest's name.
        assert_eq!(crate_name_in(&manifest(FULL)).as_deref(), Some("my-crate"));
    }

    #[test]
    fn a_manifest_without_a_package_section_has_no_crate_name() {
        assert_eq!(
            crate_name_in(&manifest("[workspace]\nmembers = [\"a\"]\n")),
            None
        );
        assert_eq!(crate_name_in(&manifest("[package]\nversion = \"1\"\n")), None);
        // A non-string name is not a name.
        assert_eq!(crate_name_in(&manifest("[package]\nname = 7\n")), None);
    }

    /// All three dependency tables count, and hyphens become underscores
    /// because that is how the crate is spelled in a `use` path.
    #[test]
    fn collects_every_dependency_table_with_underscores() {
        assert_eq!(
            sorted(dependencies_in(&manifest(FULL))),
            ["cc", "serde", "tempfile", "tree_sitter"]
        );
    }

    #[test]
    fn a_manifest_with_no_dependencies_yields_nothing() {
        assert!(dependencies_in(&manifest("[package]\nname = \"solo\"\n")).is_empty());
        // `[dependencies]` present but empty.
        assert!(dependencies_in(&manifest("[dependencies]\n")).is_empty());
        // A target-specific table is not one of the three read here.
        assert!(
            dependencies_in(&manifest("[target.'cfg(unix)'.dependencies]\nlibc = \"0\"\n"))
                .is_empty()
        );
    }

    // --- `use` path classification ---------------------------------------

    fn deps(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    #[test]
    fn the_standard_distribution_is_stdlib() {
        for path in [
            "std",
            "std::collections::HashMap",
            "core::fmt",
            "alloc::vec::Vec",
            "proc_macro",
            "test",
        ] {
            assert_eq!(classify_import(path, Some("mine"), &deps(&[])), "stdlib", "{path}");
        }
    }

    #[test]
    fn crate_self_super_and_the_own_name_are_internal() {
        let set = deps(&["serde"]);
        for path in ["crate::roots", "self::helper", "super::sibling"] {
            assert_eq!(classify_import(path, Some("callix"), &set), "internal", "{path}");
        }
        assert_eq!(classify_import("callix::ids", Some("callix"), &set), "internal");
        // The manifest spells it with a hyphen, the code with an underscore.
        assert_eq!(classify_import("my_crate::x", Some("my-crate"), &set), "internal");
    }

    #[test]
    fn a_declared_dependency_is_third_party() {
        let set = deps(&["tree_sitter", "serde"]);
        assert_eq!(classify_import("serde", Some("mine"), &set), "third_party");
        assert_eq!(classify_import("tree_sitter::Node", Some("mine"), &set), "third_party");
        // The dependency set is stored normalized, and so is the lookup.
        assert_eq!(classify_import("tree-sitter::Node", Some("mine"), &set), "third_party");
    }

    #[test]
    fn anything_else_is_unknown() {
        let set = deps(&["serde"]);
        // A transitive dependency reachable only through a re-export.
        assert_eq!(classify_import("serde_json::Value", Some("mine"), &set), "unknown");
        assert_eq!(classify_import("", Some("mine"), &set), "unknown");
        assert_eq!(classify_import("x", None, &set), "unknown");
        // A crate whose name merely starts with a dependency's is not one.
        assert_eq!(classify_import("serde_derive", Some("mine"), &set), "unknown");
    }

    /// The first segment is trimmed before it is matched, because a `use`
    /// path arrives as written in the source.
    #[test]
    fn the_first_segment_is_trimmed() {
        assert_eq!(classify_import("  std::x  ", None, &deps(&[])), "stdlib");
        assert_eq!(classify_import(" serde ", None, &deps(&["serde"])), "third_party");
    }

    #[test]
    fn the_pyfunction_wrapper_defaults_the_dependency_set() {
        assert_eq!(classify_rust_import("std::fmt", None, None), "stdlib");
        assert_eq!(classify_rust_import("serde", None, None), "unknown");
    }
}
