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

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(raw: &[&str]) -> Vec<String> {
        let mut input: Vec<PathBuf> = raw.iter().map(PathBuf::from).collect();
        sort_paths(&mut input);
        input.iter().map(|p| p.to_string_lossy().into_owned()).collect()
    }

    /// The regression this function exists for. `-` is 0x2d and `/` is 0x2f,
    /// so as whole strings `src/v4-mini/x.py` sorts first; comparing parts
    /// the way CPython does puts `src/v4/y.py` first because the shorter
    /// component is a prefix of the longer.
    #[test]
    fn sorts_component_by_component_not_by_string() {
        assert_eq!(
            sorted(&["src/v4-mini/x.py", "src/v4/y.py"]),
            ["src/v4/y.py", "src/v4-mini/x.py"]
        );
        // The divergence is real: plain string order is the other way round.
        let mut as_strings = ["src/v4-mini/x.py", "src/v4/y.py"];
        as_strings.sort_unstable();
        assert_eq!(as_strings, ["src/v4-mini/x.py", "src/v4/y.py"]);
    }

    /// Ground truth taken from CPython 3.13:
    /// `sorted(map(pathlib.Path, ...))`, which compares `_parts_normcase`.
    #[test]
    fn matches_cpython_path_ordering() {
        assert_eq!(
            sorted(&[
                "src/v4-mini/x.py",
                "src/v4/y.py",
                "src/a",
                "src/a/b",
                "src/a-b/c",
                "src/ab/c",
            ]),
            [
                "src/a",
                "src/a/b",
                "src/a-b/c",
                "src/ab/c",
                "src/v4/y.py",
                "src/v4-mini/x.py",
            ]
        );
    }

    /// A deeper path sorts BEFORE its shallower neighbour, which reads as
    /// backwards until you follow the components: `a/b.py` is `["a", "b.py"]`
    /// and `a/b/c.py` is `["a", "b", "c.py"]`, and at index 1 `"b" < "b.py"`
    /// because the shorter string is a prefix of the longer. So the whole
    /// deeper path wins.
    ///
    /// Asserting the intuitive order here would be asserting the STRING order,
    /// which is exactly the bug `path_sort_key` exists to avoid. Checked
    /// against the authority named in the invariant:
    ///
    /// ```pycon
    /// >>> sorted(Path(p) for p in ["a/b/c/d.py", "a/b.py", "a/b/c.py"])
    /// [Path('a/b/c/d.py'), Path('a/b/c.py'), Path('a/b.py')]
    /// ```
    #[test]
    fn a_deeper_path_sorts_before_its_shallower_neighbour() {
        assert_eq!(
            sorted(&["a/b/c/d.py", "a/b.py", "a/b/c.py"]),
            ["a/b/c/d.py", "a/b/c.py", "a/b.py"]
        );
        // The same inputs the other way round, to be sure it is the order that
        // decides and not the input sequence.
        assert_eq!(
            sorted(&["a/b.py", "a/b/c.py", "a/b/c/d.py"]),
            ["a/b/c/d.py", "a/b/c.py", "a/b.py"]
        );
    }

    #[test]
    fn common_prefix_is_broken_by_the_first_differing_component() {
        assert_eq!(
            sorted(&["pkg/z/a.py", "pkg/a/z.py"]),
            ["pkg/a/z.py", "pkg/z/a.py"]
        );
    }

    #[test]
    fn identical_paths_keep_their_order() {
        assert_eq!(sorted(&["a/b.py", "a/b.py"]), ["a/b.py", "a/b.py"]);
        assert_eq!(sorted(&[]), Vec::<String>::new());
        assert_eq!(sorted(&["only.py"]), ["only.py"]);
    }

    /// Absolute paths all share the leading empty component, so the ordering
    /// is the same as CPython's even though it spells that component "/".
    #[test]
    fn absolute_paths_sort_component_wise_too() {
        assert_eq!(
            sorted(&["/root/v4-mini/x.py", "/root/v4/y.py"]),
            ["/root/v4/y.py", "/root/v4-mini/x.py"]
        );
    }

    #[test]
    fn path_sort_key_splits_on_separators() {
        assert_eq!(path_sort_key(Path::new("a/b/c.py")), ["a", "b", "c.py"]);
        assert_eq!(path_sort_key(Path::new("/a/b")), ["", "a", "b"]);
        assert_eq!(path_sort_key(Path::new("solo")), ["solo"]);
    }

    #[test]
    fn excluded_dir_matches_at_any_depth() {
        let root = Path::new("/repo");
        assert!(in_excluded_dir(
            Path::new("/repo/node_modules/pkg/index.js"),
            root,
            &EXCLUDED_DIRS
        ));
        assert!(in_excluded_dir(
            Path::new("/repo/src/deep/__pycache__/m.pyc"),
            root,
            &EXCLUDED_DIRS
        ));
        assert!(!in_excluded_dir(Path::new("/repo/src/app.py"), root, &EXCLUDED_DIRS));
    }

    #[test]
    fn excluded_dir_ignores_the_search_root_and_paths_outside_it() {
        // The root's own name is never a reason to exclude: analysing a
        // directory that happens to be called `build` must still work.
        assert!(!in_excluded_dir(
            Path::new("/repo/build/main.py"),
            Path::new("/repo/build"),
            &EXCLUDED_DIRS
        ));
        // A path that is not under the root cannot be classified.
        assert!(!in_excluded_dir(
            Path::new("/elsewhere/dist/x.py"),
            Path::new("/repo"),
            &EXCLUDED_DIRS
        ));
        assert!(!in_excluded_dir(Path::new("/repo/dist/x.py"), Path::new("/repo"), &[]));
    }

    /// Root-anchoring is the whole point: `target/` at the crate root is
    /// Cargo's, while `src/target/mod.rs` is first-party code.
    #[test]
    fn excluded_root_dir_is_anchored_at_the_root() {
        let root = Path::new("/crate");
        assert!(under_excluded_root_dir(
            Path::new("/crate/target/debug/deps/x.rs"),
            root,
            &["target"]
        ));
        assert!(!under_excluded_root_dir(
            Path::new("/crate/src/target/mod.rs"),
            root,
            &["target"]
        ));
        assert!(under_excluded_root_dir(
            Path::new("/mod/vendor/github.com/x/y.go"),
            Path::new("/mod"),
            &["vendor"]
        ));
        assert!(!under_excluded_root_dir(
            Path::new("/mod/internal/vendor/y.go"),
            Path::new("/mod"),
            &["vendor"]
        ));
    }

    #[test]
    fn excluded_root_dir_excludes_nothing_without_a_list() {
        assert!(!under_excluded_root_dir(
            Path::new("/crate/target/debug/x.rs"),
            Path::new("/crate"),
            &[]
        ));
        // The root itself has no first component to match.
        assert!(!under_excluded_root_dir(
            Path::new("/crate"),
            Path::new("/crate"),
            &["target"]
        ));
        assert!(!under_excluded_root_dir(
            Path::new("/elsewhere/target/x.rs"),
            Path::new("/crate"),
            &["target"]
        ));
    }

    #[test]
    fn relative_to_includes_the_root_itself() {
        assert!(is_relative_to(Path::new("/r/a.py"), Path::new("/r")));
        assert!(is_relative_to(Path::new("/r"), Path::new("/r")));
        assert!(!is_relative_to(Path::new("/other/a.py"), Path::new("/r")));
    }

    #[test]
    fn nested_root_files_are_dropped_from_the_parent() {
        let files: Vec<PathBuf> = ["/r/a.py", "/r/sub/b.py", "/r/sub/deep/c.py"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let roots: Vec<PathBuf> = ["/r", "/r/sub"].iter().map(PathBuf::from).collect();

        assert_eq!(
            filter_nested_root_files(files.clone(), Path::new("/r"), &roots),
            [PathBuf::from("/r/a.py")]
        );
        // The nested root keeps everything: its parent is not nested in it.
        assert_eq!(
            filter_nested_root_files(files, Path::new("/r/sub"), &roots).len(),
            3
        );
    }

    #[test]
    fn ini_reads_indented_continuation_lines() {
        let text = "\
[metadata]
name = mypkg

[options]
install_requires =
    requests>=2.0
    # a hash comment
    httpx
    ; a semicolon comment
python_requires = >=3.8
";
        assert_eq!(
            ini_sections(text, "install_requires"),
            [(
                "options".to_string(),
                vec!["requests>=2.0".to_string(), "httpx".to_string()]
            )]
        );
    }

    #[test]
    fn ini_reads_an_inline_value() {
        // The value is taken verbatim — splitting on commas is the caller's
        // business.
        assert_eq!(
            ini_sections("[options]\ninstall_requires = requests, httpx\n", "install_requires"),
            [("options".to_string(), vec!["requests, httpx".to_string()])]
        );
        // An inline value plus continuations keeps both.
        assert_eq!(
            ini_sections(
                "[options]\ninstall_requires = requests\n    httpx\n",
                "install_requires"
            ),
            [(
                "options".to_string(),
                vec!["requests".to_string(), "httpx".to_string()]
            )]
        );
    }

    #[test]
    fn ini_matches_keys_case_insensitively_and_sections_case_sensitively() {
        assert_eq!(
            ini_sections("[options]\nINSTALL_REQUIRES =\n    flask\n", "install_requires"),
            [("options".to_string(), vec!["flask".to_string()])]
        );
        // The section name is reported as written, so a caller comparing it
        // to "options" finds nothing here — which is the behaviour
        // `parse_setup_cfg` relies on.
        assert_eq!(
            ini_sections("[Options]\ninstall_requires =\n    flask\n", "install_requires"),
            [("Options".to_string(), vec!["flask".to_string()])]
        );
    }

    #[test]
    fn ini_reports_every_matching_section_separately() {
        let text = "\
[options]
install_requires =
    requests
[options.extras_require]
install_requires =
    pytest
";
        assert_eq!(
            ini_sections(text, "install_requires"),
            [
                ("options".to_string(), vec!["requests".to_string()]),
                ("options.extras_require".to_string(), vec!["pytest".to_string()]),
            ]
        );
    }

    #[test]
    fn ini_yields_nothing_when_the_key_is_absent() {
        assert!(ini_sections("[options]\npython_requires = >=3.8\n", "install_requires").is_empty());
        assert!(ini_sections("", "install_requires").is_empty());
        assert!(ini_sections("[metadata]\nname = x\n", "install_requires").is_empty());
    }

    #[test]
    fn ini_records_a_key_with_no_value_at_all() {
        // `install_requires =` followed by nothing is an empty list, not an
        // absent key: the section still shows up.
        assert_eq!(
            ini_sections("[options]\ninstall_requires =\n", "install_requires"),
            [("options".to_string(), Vec::<String>::new())]
        );
    }

    #[test]
    fn ini_survives_malformed_input() {
        // An unterminated header is not a section, so the key that follows
        // lands in whatever section was open before (here: none).
        assert_eq!(
            ini_sections("[options\ninstall_requires = requests\n", "install_requires"),
            [(String::new(), vec!["requests".to_string()])]
        );
        // A line that is neither a section nor a `key = value` does not end
        // the value being collected — the continuation after it still counts.
        assert_eq!(
            ini_sections(
                "[options]\ninstall_requires =\n    requests\ngarbage\n    httpx\n",
                "install_requires"
            ),
            [(
                "options".to_string(),
                vec!["requests".to_string(), "httpx".to_string()]
            )]
        );
        // A tab is indentation too.
        assert_eq!(
            ini_sections("[options]\ninstall_requires =\n\trequests\n", "install_requires"),
            [("options".to_string(), vec!["requests".to_string()])]
        );
        // Only the first `=` splits the pair.
        assert_eq!(
            ini_sections("[options]\ninstall_requires = a==1.0\n", "install_requires"),
            [("options".to_string(), vec!["a==1.0".to_string()])]
        );
    }

    #[test]
    fn ini_takes_the_key_before_any_section() {
        assert_eq!(
            ini_sections("install_requires = requests\n", "install_requires"),
            [(String::new(), vec!["requests".to_string()])]
        );
    }
}
