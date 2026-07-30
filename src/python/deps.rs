//! Manifest parsing: where third-party package names come from.
//!
//! Dev/test groups are included deliberately — otherwise imports from tests
//! would classify as `unknown` rather than `third_party`.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::roots::ini_sections;

/// Normalizes a distribution name into a form comparable with an import
/// name.
///
/// Strips versions and extras (`requests>=2.0 [security]` → `requests`),
/// lowercases, and turns hyphens into underscores. Scoped npm names
/// (`@scope/pkg`) are left as they are.
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

/// PEP 621 (`[project]`) and Poetry (`[tool.poetry]`) from pyproject.toml.
fn parse_pyproject(root: &Path, names: &mut HashSet<String>) {
    let path = root.join("pyproject.toml");
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return;
    };
    collect_pyproject(&table, names);
}

/// The traversal half of [`parse_pyproject`]. Split from the read because the
/// interesting part is the shape of the manifest — five places declare
/// dependencies and each nests differently — not the file access.
fn collect_pyproject(table: &toml::Table, names: &mut HashSet<String>) {
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
    // poetry >= 1.2 dependency groups
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

/// URL/VCS lines are skipped: no import name can be pulled from them.
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
            // `-r other.txt` includes are expanded one level deep.
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

/// The `requirements*.txt` files at the project root.
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

/// `install_requires` from `[options]` and `[options.extras_require*]`.
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

/// Third-party package names declared in the root's manifests.
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

/// Top-level stdlib module names for the running interpreter.
///
/// Taken from `sys.stdlib_module_names` — a property of the running Python
/// rather than of the build, so it is computed on the interpreter side.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn collected(text: &str) -> Vec<String> {
        let table = text.parse::<toml::Table>().expect("the test manifest parses");
        let mut names = HashSet::new();
        collect_pyproject(&table, &mut names);
        let mut out: Vec<String> = names.into_iter().collect();
        out.sort();
        out
    }

    #[test]
    fn normalizes_a_requirement_into_an_import_name() {
        assert_eq!(normalize_pkg_name("requests>=2.0"), "requests");
        assert_eq!(normalize_pkg_name("requests [security]"), "requests");
        assert_eq!(normalize_pkg_name("Flask-SQLAlchemy"), "flask_sqlalchemy");
        assert_eq!(normalize_pkg_name("pytest;python_version>'3'"), "pytest");
        assert_eq!(normalize_pkg_name("UPPER"), "upper");
        // Every version specifier PEP 508 allows.
        for raw in ["a==1", "a>=1", "a<=1", "a>1", "a<1", "a!=1", "a~=1", "a===1"] {
            assert_eq!(normalize_pkg_name(raw), "a", "{raw}");
        }
    }

    #[test]
    fn normalizes_nothing_out_of_a_comment_or_blank() {
        assert_eq!(normalize_pkg_name(""), "");
        assert_eq!(normalize_pkg_name("   "), "");
        assert_eq!(normalize_pkg_name("# comment"), "");
        assert_eq!(normalize_pkg_name("package # trailing"), "package");
    }

    /// An npm scope survives: `@scope/pkg` is one package name, and turning
    /// its hyphens into underscores would break the lookup against
    /// package.json. Shared with the TypeScript side, which is why it lives
    /// here rather than in the Python-only path.
    #[test]
    fn a_scoped_npm_name_keeps_its_shape() {
        assert_eq!(normalize_pkg_name("@scope/Pkg"), "@scope/pkg");
        assert_eq!(normalize_pkg_name("@types/node"), "@types/node");
        assert_eq!(normalize_pkg_name("@my-scope/my-pkg"), "@my-scope/my-pkg");
    }

    #[test]
    fn reads_pep_621_dependencies_and_every_extra_group() {
        let text = r#"
[project]
name = "demo"
dependencies = ["requests>=2.0", "Flask-SQLAlchemy"]

[project.optional-dependencies]
dev = ["mypy", "ruff"]
docs = ["sphinx"]
"#;
        assert_eq!(
            collected(text),
            ["flask_sqlalchemy", "mypy", "requests", "ruff", "sphinx"]
        );
    }

    /// Poetry declares dependencies as table *keys*, and `python` is a
    /// constraint on the interpreter rather than an importable package.
    #[test]
    fn reads_poetry_groups_and_drops_the_python_constraint() {
        let text = r#"
[tool.poetry]
name = "p"

[tool.poetry.dependencies]
python = "^3.13"
requests = "^2"
Flask-Login = "*"

[tool.poetry.dev-dependencies]
pytest = "*"

[tool.poetry.group.test.dependencies]
hypothesis = "*"

[tool.poetry.group.lint.dependencies]
ruff = "*"
"#;
        assert_eq!(
            collected(text),
            ["flask_login", "hypothesis", "pytest", "requests", "ruff"]
        );
    }

    #[test]
    fn a_manifest_declaring_nothing_yields_nothing() {
        assert!(collected("[tool.other]\nx = 1\n").is_empty());
        assert!(collected("[project]\nname = \"x\"\n").is_empty());
        // A scalar where a list belongs is ignored rather than coerced.
        assert!(collected("[project]\ndependencies = \"requests\"\n").is_empty());
        assert!(collected("[tool.poetry]\nname = \"x\"\n").is_empty());
    }

    /// Both layouts in one file: a Poetry project migrating to PEP 621 has
    /// both, and neither may shadow the other.
    #[test]
    fn pep_621_and_poetry_are_both_read() {
        let text = r#"
[project]
dependencies = ["sphinx"]

[tool.poetry.dependencies]
requests = "^2"
"#;
        assert_eq!(collected(text), ["requests", "sphinx"]);
    }

    #[test]
    fn requirement_lines_that_carry_no_import_name_are_skipped() {
        for line in [
            "-r other.txt",
            "-c constraints.txt",
            "-e .",
            "https://example.com/x.whl",
            "http://example.com/x.whl",
            "git+https://github.com/a/b",
            "svn+https://example.com/x",
            "hg+https://example.com/x",
            "bzr+https://example.com/x",
            "  -r indented.txt",
        ] {
            assert!(is_skipped(line), "{line:?} should be skipped");
        }
        for line in ["requests", "flask>=2", "-- not a flag we know", "editable"] {
            assert!(!is_skipped(line), "{line:?} should be kept");
        }
    }
}
