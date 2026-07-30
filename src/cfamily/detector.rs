//! Detecting C and C++ projects, and collecting their sources.
//!
//! There is no manifest to read. A C or C++ project declares how it is BUILT,
//! never what it is: CMake, make, meson and autotools are all used by both
//! dialects, and none of them says which one a directory holds. So markers only
//! drive root discovery, and the predicates answer "is there code of this
//! dialect here" from the extensions themselves — which is the only way to tell
//! a C project from a C++ one when both ship a `CMakeLists.txt`.

use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use walkdir::WalkDir;

use crate::roots::{EXCLUDED_DIRS, collect_files, collect_marker_roots};

use super::Dialect;

/// Build systems, not languages: a marker says where a project starts.
const C_FAMILY_MARKERS: [&str; 4] =
    ["CMakeLists.txt", "Makefile", "meson.build", "configure.ac"];

pub const C_SOURCE_EXTENSIONS: [&str; 1] = [".c"];
pub const CPP_SOURCE_EXTENSIONS: [&str; 4] = [".cpp", ".cc", ".cxx", ".c++"];
/// `.h` names no dialect; `headers::probe_dialect` decides per file.
pub const AMBIGUOUS_HEADER_EXTENSIONS: [&str; 1] = [".h"];
pub const CPP_HEADER_EXTENSIONS: [&str; 4] = [".hpp", ".hh", ".hxx", ".h++"];

/// Everything the C adapter may be handed.
pub const C_EXTENSIONS: [&str; 2] = [".c", ".h"];

/// Everything the C++ adapter may be handed. `.h` is here too: both adapters
/// COLLECT it and exactly one of them keeps a given file (see `headers::claims`).
pub const CPP_EXTENSIONS: [&str; 9] = [
    ".cpp", ".cc", ".cxx", ".c++", ".hpp", ".hh", ".hxx", ".h++", ".h",
];

/// Out-of-source build trees, matched at any depth. `build` and `.git` already
/// come from `EXCLUDED_DIRS`; `_build` is meson's and autotools' habit.
const C_SKIP_ANYWHERE: [&str; 1] = ["_build"];

/// A vendored tree is written at the project root and nowhere else. Anchoring
/// matters here more than for most languages: `external` and `include/vendor`
/// are perfectly ordinary first-party directory names deeper in a C tree.
const C_SKIP_AT_ROOT: [&str; 4] = ["vendor", "third_party", "external", "subprojects"];

/// CLion and every cmake wrapper write `cmake-build-debug`,
/// `cmake-build-release` and friends. The name carries a suffix, so it cannot be
/// one of `collect_files`'s exact-match entries.
const CMAKE_BUILD_PREFIX: &str = "cmake-build";

/// Whether one of the path's DIRECTORIES is a cmake build tree.
///
/// The file name is excluded from the test on purpose: `cmake-build.c` is a
/// source file, not a build directory.
fn in_cmake_build_dir(path: &Path, root: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    relative.parent().is_some_and(|dirs| {
        dirs.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .starts_with(CMAKE_BUILD_PREFIX)
        })
    })
}

/// Whether the path's extension is one of the given set.
pub fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension().is_some_and(|ext| {
        let dotted = format!(".{}", ext.to_string_lossy());
        extensions.contains(&dotted.as_str())
    })
}

/// Sources with the given extensions under the root.
fn collect_family_files(root: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let excluded: Vec<&str> = EXCLUDED_DIRS
        .iter()
        .chain(C_SKIP_ANYWHERE.iter())
        .copied()
        .collect();
    let mut files = collect_files(root, extensions, &excluded, &C_SKIP_AT_ROOT);
    // `collect_files` sorted them and `retain` keeps that order.
    files.retain(|path| !in_cmake_build_dir(path, root));
    files
}

/// C sources under the root: `.c` plus every `.h`, since a header's dialect is
/// only known once it has been read.
pub fn collect_c_files(root: &Path) -> Vec<PathBuf> {
    collect_family_files(root, &C_EXTENSIONS)
}

/// C++ sources under the root, `.h` included for the same reason.
pub fn collect_cpp_files(root: &Path) -> Vec<PathBuf> {
    collect_family_files(root, &CPP_EXTENSIONS)
}

/// Whether any file with one of the extensions lives under the root.
///
/// One walk for the whole set: `roots::any_file_with_extension` takes a single
/// extension, and asking it nine times would walk the tree nine times.
fn any_source(root: &Path, extensions: &[&str]) -> bool {
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .any(|entry| entry.file_type().is_file() && has_extension(entry.path(), extensions))
}

fn any_cpp_source(root: &Path) -> bool {
    any_source(root, &CPP_SOURCE_EXTENSIONS) || any_source(root, &CPP_HEADER_EXTENSIONS)
}

/// Whether the root holds C.
///
/// A build marker is not enough: it would make every C++ project a C project as
/// well. A tree of nothing but `.h` counts as C only when nothing C++ is present
/// — a header-only C library has to belong to somebody, and that is the same
/// tie-break `headers::probe_dialect` applies file by file.
pub fn is_c(root: &Path) -> bool {
    any_source(root, &C_SOURCE_EXTENSIONS)
        || (any_source(root, &AMBIGUOUS_HEADER_EXTENSIONS) && !any_cpp_source(root))
}

/// Whether the root holds C++.
///
/// Only the extensions that name C++ on their own count. A mixed repository is
/// legitimately both a C and a C++ project, and both adapters then run over the
/// files each one claims.
pub fn is_cpp(root: &Path) -> bool {
    any_cpp_source(root)
}

fn holds(root: &Path, dialect: Dialect) -> bool {
    match dialect {
        Dialect::C => is_c(root),
        Dialect::Cpp => is_cpp(root),
    }
}

/// Project roots of one dialect when walking a monorepo.
///
/// A directory with a marker but no code of the dialect is not a root of it, or
/// a C++ subproject's `CMakeLists.txt` would open an empty C root beside it. The
/// shared helper's fallback to `search_root` itself is kept when that root does
/// hold code, because a tree of loose sources with no build system at all is
/// still a project — and dropped when it does not, so the caller gets an empty
/// list rather than a root with nothing in it.
fn family_roots(root: &Path, dialect: Dialect) -> Vec<PathBuf> {
    let excluded: Vec<&str> = EXCLUDED_DIRS
        .iter()
        .chain(C_SKIP_ANYWHERE.iter())
        .copied()
        .collect();
    let roots = collect_marker_roots(root, &C_FAMILY_MARKERS, &excluded, |marker| {
        marker.parent().is_some_and(|dir| holds(dir, dialect))
    });
    if roots.len() == 1 && roots[0] == root && !holds(root, dialect) {
        return Vec::new();
    }
    roots
}

pub fn c_roots(root: &Path) -> Vec<PathBuf> {
    family_roots(root, Dialect::C)
}

pub fn cpp_roots(root: &Path) -> Vec<PathBuf> {
    family_roots(root, Dialect::Cpp)
}

/// The name from CMake's `project()` command.
///
/// Read case-insensitively, because CMake commands are, and only as far as the
/// first argument, because that is where the name is. A `${VAR}` there is
/// rejected rather than reported verbatim: a node's project name is part of
/// every id in the graph and has to be a literal.
pub fn cmake_project_name(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join("CMakeLists.txt")).ok()?;
    for line in text.lines() {
        let trimmed = line.trim_start();
        // `get` rather than a slice: a line may start with a multi-byte
        // character and slicing it would panic.
        let Some(head) = trimmed.get(.."project".len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case("project") {
            continue;
        }
        // `project (foo)` is as legal as `project(foo)`.
        let Some(arguments) = trimmed["project".len()..].trim_start().strip_prefix('(') else {
            continue;
        };
        let first = arguments.split(')').next().unwrap_or(arguments);
        let Some(name) = first.split_whitespace().next() else {
            continue;
        };
        if name.is_empty() || name.contains('$') {
            continue;
        }
        return Some(name.trim_matches('"').to_string());
    }
    None
}

/// The project name — CMake's, else the directory's.
pub fn project_name(root: &Path) -> String {
    cmake_project_name(root).unwrap_or_else(|| {
        root.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn is_c_project(root: PathBuf) -> bool {
    is_c(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn find_c_roots(root: PathBuf) -> Vec<PathBuf> {
    c_roots(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn c_detect_project_name(root: PathBuf) -> String {
    project_name(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn is_cpp_project(root: PathBuf) -> bool {
    is_cpp(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn find_cpp_roots(root: PathBuf) -> Vec<PathBuf> {
    cpp_roots(&root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn cpp_detect_project_name(root: PathBuf) -> String {
    project_name(&root)
}
