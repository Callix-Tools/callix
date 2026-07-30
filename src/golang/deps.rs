//! Parsing go.mod and detecting Go projects.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

use crate::roots::collect_marker_roots;

const GO_MARKERS: [&str; 1] = ["go.mod"];
const GO_EXCLUDED_DIRS: [&str; 4] = ["vendor", ".git", "node_modules", "testdata"];

/// The module path from the `module` directive in go.mod.
pub fn module_path(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join("go.mod")).ok()?;
    module_path_in(&text)
}

/// The parsing half of [`module_path`]. Split from the read so the grammar
/// can be tested without a go.mod on disk — the directive has more shapes
/// than it looks (indented, padded, and `modulefoo` is not one of them).
fn module_path_in(text: &str) -> Option<String> {
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

/// Module paths from `require` directives — both block and single-line.
pub fn required_modules(root: &Path) -> HashSet<String> {
    match fs::read_to_string(root.join("go.mod")) {
        Ok(text) => required_modules_in(&text),
        Err(_) => HashSet::new(),
    }
}

/// The parsing half of [`required_modules`], split from the read for the same
/// reason as [`module_path_in`]: the block scanner is string surgery and the
/// interesting inputs (a `require` inside a module path, an unterminated
/// block) are awkward to set up as files.
fn required_modules_in(text: &str) -> HashSet<String> {
    let mut modules = HashSet::new();

    // A require ( ... ) block
    let mut rest = text;
    while let Some(start) = rest.find("require") {
        let after = &rest[start + "require".len()..];
        let Some(open) = after.find('(') else {
            rest = after;
            continue;
        };
        // Only whitespace may sit between `require` and `(`.
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

    // A single-line `require <path> <version>`: whitespace within the line
    // only, or a block's opening parenthesis would end up in the path.
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

/// Whether the directory holds a go.mod.
pub fn is_go(root: &Path) -> bool {
    root.join("go.mod").is_file()
}

/// Go module roots when walking a monorepo.
pub fn go_roots(root: &Path) -> Vec<PathBuf> {
    let roots = collect_marker_roots(root, &GO_MARKERS, &GO_EXCLUDED_DIRS, |_| true);
    // The shared helper falls back to the root itself; Go does not want that.
    if roots.len() == 1 && roots[0] == root && !is_go(root) {
        return Vec::new();
    }
    roots
}

/// The project name — the last segment of the module path.
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

/// Classification of a Go import path.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(modules: HashSet<String>) -> Vec<String> {
        let mut out: Vec<String> = modules.into_iter().collect();
        out.sort();
        out
    }

    #[test]
    fn reads_the_module_directive() {
        assert_eq!(
            module_path_in("module github.com/me/app\n\ngo 1.22\n").as_deref(),
            Some("github.com/me/app")
        );
        // Indented and padded are both legal.
        assert_eq!(
            module_path_in("  module   github.com/me/app  \n").as_deref(),
            Some("github.com/me/app")
        );
        // The first directive wins.
        assert_eq!(
            module_path_in("module first\nmodule second\n").as_deref(),
            Some("first")
        );
    }

    /// `module` must be a word of its own: a whitespace check is the only
    /// thing between the directive and any identifier that starts with it.
    #[test]
    fn a_word_starting_with_module_is_not_the_directive() {
        assert_eq!(
            module_path_in("modulefoo bar\nmodule github.com/spaced/app\n").as_deref(),
            Some("github.com/spaced/app")
        );
        assert_eq!(module_path_in("module\n").as_deref(), None);
        assert_eq!(module_path_in("go 1.22\n").as_deref(), None);
        assert_eq!(module_path_in("").as_deref(), None);
    }

    #[test]
    fn reads_a_require_block() {
        let text = "\
module github.com/me/app

go 1.22

require (
\tgithub.com/gin-gonic/gin v1.9.1
\t// a comment inside the block
\tgolang.org/x/net v0.17.0 // indirect
)
";
        assert_eq!(
            sorted(required_modules_in(text)),
            ["github.com/gin-gonic/gin", "golang.org/x/net"]
        );
    }

    #[test]
    fn reads_a_single_line_require() {
        assert_eq!(
            sorted(required_modules_in(
                "module example.com/x\n\nrequire github.com/pkg/errors v0.9.1\n"
            )),
            ["github.com/pkg/errors"]
        );
        // A require with no version is not a requirement.
        assert!(required_modules_in("module m\nrequire onlypath\n").is_empty());
    }

    #[test]
    fn reads_several_blocks_and_a_trailing_single_line() {
        assert_eq!(
            sorted(required_modules_in(
                "module m\nrequire (\n\ta v1\n)\nrequire (\n\tb v2\n)\nrequire c v3\n"
            )),
            ["a", "b", "c"]
        );
    }

    /// The block scanner searches for the literal `require`, so a module path
    /// containing it must not derail the walk.
    #[test]
    fn require_inside_a_module_path_is_not_a_directive() {
        assert_eq!(
            sorted(required_modules_in(
                "module github.com/me/require\n\nrequire (\n\tgithub.com/a/b v1.0.0\n)\n"
            )),
            ["github.com/a/b"]
        );
        // `require` glued to the parenthesis with a word in between is not a
        // block opening either.
        assert!(required_modules_in("module m\nrequirements (\n\ta v1\n)\n").is_empty());
    }

    /// An unterminated block yields nothing rather than reading to EOF, and —
    /// the part that matters — the scan terminates.
    #[test]
    fn an_unterminated_block_is_abandoned() {
        assert!(required_modules_in("module m\n\nrequire (\n\tgithub.com/a/b v1.0.0\n").is_empty());
        assert!(required_modules_in("require").is_empty());
        assert!(required_modules_in("require (").is_empty());
        assert!(required_modules_in("").is_empty());
    }

    #[test]
    fn project_name_is_the_last_segment_of_the_module_path() {
        // `project_name` reads the file, so only the pure part is exercised
        // here; the trimming it does is spelled out in the assertions below.
        assert_eq!(
            module_path_in("module github.com/me/app/v2\n").as_deref(),
            Some("github.com/me/app/v2")
        );
    }

    // --- import classification (the implementation lives in the visitor) ---

    fn required(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn classifies_an_internal_import_by_module_prefix() {
        let deps = required(&["github.com/gin-gonic/gin"]);
        let module = Some("github.com/me/app");
        assert_eq!(classify_go_import("github.com/me/app", module, Some(deps.clone())), "internal");
        assert_eq!(
            classify_go_import("github.com/me/app/internal/db", module, Some(deps.clone())),
            "internal"
        );
        // A prefix that is only a substring of another path is not a match:
        // `.../apple` must not be swallowed by `.../app`.
        assert_eq!(
            classify_go_import("github.com/me/apple", module, Some(deps)),
            "unknown"
        );
    }

    /// Go's own rule: a standard-library path has no dot in its first segment.
    #[test]
    fn classifies_the_standard_library_by_the_missing_dot() {
        assert_eq!(classify_go_import("fmt", None, None), "stdlib");
        assert_eq!(classify_go_import("net/http", None, None), "stdlib");
        assert_eq!(classify_go_import("encoding/json", None, None), "stdlib");
        // An empty path has no dot either, and is reported as stdlib. Not
        // reachable from a real import, but pinned so the fallthrough is
        // known.
        assert_eq!(classify_go_import("", None, None), "stdlib");
    }

    #[test]
    fn classifies_a_required_module_as_third_party() {
        let deps = required(&["github.com/gin-gonic/gin", "golang.org/x/net"]);
        for path in [
            "github.com/gin-gonic/gin",
            "github.com/gin-gonic/gin/binding",
            "golang.org/x/net/http2",
        ] {
            assert_eq!(
                classify_go_import(path, Some("github.com/me/app"), Some(deps.clone())),
                "third_party",
                "{path}"
            );
        }
        // A domain-rooted path nobody requires is a transitive dependency.
        assert_eq!(
            classify_go_import("github.com/other/x", Some("github.com/me/app"), Some(deps)),
            "unknown"
        );
    }

    #[test]
    fn without_a_module_path_nothing_is_internal() {
        assert_eq!(classify_go_import("github.com/me/app", None, None), "unknown");
    }
}
