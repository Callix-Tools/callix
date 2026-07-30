//! Dependencies of a C or C++ project, read from whatever build files it has.
//!
//! **This is best-effort, and the honest statement of the limit comes first.**
//! C and C++ have no universal manifest. There is no `go.mod`, no
//! `package.json`, no `Cargo.toml` — a dependency is whatever the build system
//! happens to link, and the build system is CMake, meson, autotools, plain
//! make, bazel, or a shell script. Worse, nothing links a header to a package:
//! `#include <curl/curl.h>` resolves through the compiler's include path, and a
//! project that installs libcurl system-wide and never mentions it in any file
//! we can read has a dependency this module CANNOT see. Pretending otherwise —
//! guessing a package from a header name — would invent DEPENDENCY nodes that
//! no manifest supports, so it is not done.
//!
//! What IS read, because it is a declaration rather than a guess:
//!
//! | File | What counts as a dependency |
//! |---|---|
//! | `CMakeLists.txt` | `find_package`, `pkg_check_modules`, `FetchContent_Declare`, the libraries of `target_link_libraries` that this project does not define itself, and a vendored `add_subdirectory` |
//! | `vcpkg.json` | `dependencies`, including each feature's |
//! | `conanfile.txt` | `[requires]`, `[tool_requires]`, `[build_requires]` |
//! | `conanfile.py` | the `requires` attribute and `self.requires(...)` calls |
//! | `meson.build` | `dependency('…')` |
//!
//! Nothing here evaluates a variable. `target_link_libraries(app ${LIBS})` names
//! a dependency that only a configure run knows, and a node called `${LIBS}`
//! would be worse than no node at all, so any argument holding a `$` is dropped.

use std::collections::HashSet;
use std::fs;
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::str::CharIndices;

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use serde_json::Value;

/// How far `add_subdirectory` is followed.
///
/// A real project nests three or four levels; the bound is there so a listfile
/// that includes its own directory cannot loop, which the `seen` set already
/// prevents — this only keeps the work finite on a pathological tree.
const MAX_SUBDIRECTORY_DEPTH: usize = 4;

/// Directory names that hold a vendored dependency.
///
/// The same names `detector::C_SKIP_AT_ROOT` refuses to collect sources from:
/// a tree under one of these is somebody else's code, so
/// `add_subdirectory(third_party/fmt)` declares a dependency on `fmt` while
/// `add_subdirectory(src/engine)` declares nothing.
const VENDOR_DIRS: [&str; 7] = [
    "vendor",
    "vendored",
    "third_party",
    "thirdparty",
    "external",
    "subprojects",
    "deps",
];

/// Arguments of `target_link_libraries` and `pkg_check_modules` that are not
/// libraries.
///
/// CMake spells its keywords in upper case by convention but compares them
/// exactly, so the comparison here is exact too.
const LINK_KEYWORDS: [&str; 14] = [
    "PRIVATE",
    "PUBLIC",
    "INTERFACE",
    "LINK_PRIVATE",
    "LINK_PUBLIC",
    "LINK_INTERFACE_LIBRARIES",
    "REQUIRED",
    "QUIET",
    "OPTIONAL",
    "IMPORTED_TARGET",
    "GLOBAL",
    "NO_CMAKE_PATH",
    "debug",
    "optimized",
];

/// CMake commands that name something this project builds itself.
///
/// They are what makes `target_link_libraries(app PRIVATE engine curl)`
/// answerable: `engine` is declared here and `curl` is not.
/// `add_subdirectory` is in the list because a library built from a
/// subdirectory of this project is linked by target name, so it is not a
/// package — the cost is that `add_subdirectory(fmt)` for an unprefixed
/// vendored copy hides `fmt`, which is the same ambiguity `VENDOR_DIRS`
/// exists to resolve.
const TARGET_COMMANDS: [&str; 4] = [
    "add_library",
    "add_executable",
    "add_custom_target",
    "add_subdirectory",
];

/// One `command(argument …)` invocation of a listfile.
struct Command {
    /// Lowercased: CMake command names are case-insensitive, and meson's are
    /// lower case already.
    name: String,
    args: Vec<String>,
}

fn is_name_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Every `command(…)` in a CMake or meson file, with its arguments split.
///
/// A scanner rather than line matching, because both facts that break line
/// matching are routine: `target_link_libraries(` is spread over five lines in
/// half the projects that use it, and a `#` inside a quoted argument is not a
/// comment. Both quote characters are honoured so the one scanner serves
/// CMake (`"…"`) and meson (`'…'`).
fn commands(text: &str) -> Vec<Command> {
    let mut out: Vec<Command> = Vec::new();
    let mut chars = text.char_indices().peekable();
    // The name may be glued to the parenthesis or separated from it by
    // whitespace — `project(foo)` and `project (foo)` are the same command —
    // so the last completed word is kept alongside the one being read.
    let mut word = String::new();
    let mut previous = String::new();

    while let Some((_, ch)) = chars.next() {
        match ch {
            '#' => {
                skip_to_end_of_line(&mut chars);
                word.clear();
                previous.clear();
            }
            // A quoted argument is data: `set(X "if(a)")` holds no command.
            '"' | '\'' => {
                skip_quoted(&mut chars, ch);
                word.clear();
                previous.clear();
            }
            '(' => {
                let name = if word.is_empty() {
                    std::mem::take(&mut previous)
                } else {
                    std::mem::take(&mut word)
                };
                if !name.is_empty() {
                    // Read from a CLONE, so the outer scan continues INSIDE the
                    // argument list. meson nests calls —
                    // `executable(…, dependencies: [dependency('z')])` — and a
                    // scan that stepped over an argument list would never see
                    // the inner one.
                    let mut lookahead = chars.clone();
                    let args = read_arguments(&mut lookahead);
                    out.push(Command { name: name.to_lowercase(), args });
                }
            }
            c if is_name_char(c) => word.push(c),
            c if c.is_whitespace() => {
                if !word.is_empty() {
                    previous = std::mem::take(&mut word);
                }
            }
            _ => {
                word.clear();
                previous.clear();
            }
        }
    }
    out
}

fn skip_to_end_of_line(chars: &mut Peekable<CharIndices<'_>>) {
    while let Some(&(_, ch)) = chars.peek() {
        if ch == '\n' {
            return;
        }
        chars.next();
    }
}

/// Consumes the rest of a quoted run, its closing delimiter included.
fn skip_quoted(chars: &mut Peekable<CharIndices<'_>>, delimiter: char) {
    while let Some((_, ch)) = chars.next() {
        match ch {
            '\\' => {
                chars.next();
            }
            c if c == delimiter => return,
            _ => {}
        }
    }
}

/// The arguments up to the closing parenthesis, whitespace-separated.
///
/// Nesting is tracked because a generator expression and an `if` condition both
/// hold balanced parentheses; the delimiters of a quoted argument are dropped
/// while its content is kept whole, so `"a b"` stays one argument.
fn read_arguments(chars: &mut Peekable<CharIndices<'_>>) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth = 1usize;
    let mut quote: Option<char> = None;

    while let Some((_, ch)) = chars.next() {
        if let Some(delimiter) = quote {
            match ch {
                '\\' => {
                    if let Some((_, escaped)) = chars.next() {
                        current.push(escaped);
                    }
                }
                c if c == delimiter => quote = None,
                c => current.push(c),
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '#' => skip_to_end_of_line(chars),
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                current.push(ch);
            }
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// A dependency name from one build-file argument, or None when the argument
/// names no package.
///
/// `CURL::libcurl` reduces to `CURL`: an imported target is namespaced by the
/// package that provides it, and the package is what a DEPENDENCY node means.
/// `-lm` reduces to `m` for the same reason. A version constraint
/// (`libcurl>=7.0`) is cut off, because two projects pinning different versions
/// depend on the same thing.
fn sanitize(raw: &str) -> Option<String> {
    // The separator comes off BEFORE the quotes. An argument list yields
    // `'zlib',` with the comma attached, and stripping quotes first leaves the
    // closing one behind — `trim_matches` only removes a character it finds at
    // the end, and there it finds the comma.
    let mut name = raw
        .trim()
        .trim_matches(',')
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    if name.is_empty() || name.contains('$') || LINK_KEYWORDS.contains(&name) {
        return None;
    }
    if let Some(stripped) = name.strip_prefix("-l") {
        name = stripped;
    }
    // A linker flag or a path is not a package name.
    if name.starts_with('-') || name.contains('/') || name.contains('\\') {
        return None;
    }
    if let Some((package, _target)) = name.split_once("::") {
        name = package;
    }
    let name = name
        .split(['<', '>', '=', '!', '~', '^', '@'])
        .next()
        .unwrap_or(name)
        .trim();
    if name.is_empty() || !name.chars().next().is_some_and(char::is_alphanumeric) {
        return None;
    }
    Some(name.to_string())
}

/// The last component of a vendored `add_subdirectory` path, when it is one.
///
/// The first component decides: `third_party/fmt` is a dependency named `fmt`,
/// while `src/engine` is this project's own code and names nothing.
fn vendored_name(path: &str) -> Option<String> {
    let mut parts = path.split(['/', '\\']).filter(|part| !part.is_empty());
    let first = parts.next()?;
    if !VENDOR_DIRS.contains(&first) {
        return None;
    }
    let last = path
        .split(['/', '\\'])
        .rfind(|part| !part.is_empty() && *part != ".")?;
    sanitize(last)
}

/// One listfile's dependency names, and the subdirectories to follow.
///
/// Split from the file reading so the grammar can be exercised without a tree
/// on disk — which is where the interesting inputs are: a multi-line
/// `target_link_libraries`, a `#` in a quoted path, a library that is one of
/// this project's own targets.
fn cmake_entries(text: &str) -> (HashSet<String>, Vec<String>) {
    let commands = commands(text);

    // The project's own targets first: a linked library that this listfile
    // declares is internal, and `target_link_libraries(app PRIVATE engine)`
    // must not report `engine` as a third-party package.
    let mut targets: HashSet<String> = HashSet::new();
    for command in &commands {
        if TARGET_COMMANDS.contains(&command.name.as_str())
            && let Some(first) = command.args.first()
            && !first.contains('$')
        {
            targets.insert(first.clone());
        }
    }

    let mut names: HashSet<String> = HashSet::new();
    let mut subdirectories: Vec<String> = Vec::new();
    for command in &commands {
        match command.name.as_str() {
            "find_package" | "fetchcontent_declare" | "find_dependency" => {
                if let Some(name) = command.args.first().and_then(|arg| sanitize(arg)) {
                    names.insert(name);
                }
            }
            // `pkg_check_modules(CURL REQUIRED libcurl)`: the first argument is
            // the variable prefix the results land in, not a package.
            "pkg_check_modules" | "pkg_search_module" => {
                for arg in command.args.iter().skip(1) {
                    if let Some(name) = sanitize(arg) {
                        names.insert(name);
                    }
                }
            }
            // The first argument is the target being linked, the rest are what
            // it links against.
            "target_link_libraries" | "link_libraries" => {
                let skip = usize::from(command.name == "target_link_libraries");
                for arg in command.args.iter().skip(skip) {
                    if targets.contains(arg) {
                        continue;
                    }
                    if let Some(name) = sanitize(arg) {
                        names.insert(name);
                    }
                }
            }
            "add_subdirectory" => {
                if let Some(path) = command.args.first().filter(|arg| !arg.contains('$')) {
                    if let Some(name) = vendored_name(path) {
                        names.insert(name);
                    }
                    subdirectories.push(path.clone());
                }
            }
            _ => {}
        }
    }
    (names, subdirectories)
}

/// Dependency names from a listfile and every subdirectory it adds.
fn cmake_dependencies(
    file: &Path,
    depth: usize,
    seen: &mut HashSet<PathBuf>,
    out: &mut HashSet<String>,
) {
    if depth > MAX_SUBDIRECTORY_DEPTH || !seen.insert(file.to_path_buf()) {
        return;
    }
    let Ok(text) = fs::read_to_string(file) else {
        return;
    };
    let (names, subdirectories) = cmake_entries(&text);
    out.extend(names);

    let Some(directory) = file.parent() else {
        return;
    };
    for subdirectory in subdirectories {
        let nested = directory.join(subdirectory).join("CMakeLists.txt");
        if nested.is_file() {
            cmake_dependencies(&nested, depth + 1, seen, out);
        }
    }
}

/// `dependencies` from a vcpkg manifest, features included.
///
/// An entry is either a bare string or an object with a `name`; both spellings
/// mean the same package, and a real manifest mixes them.
fn vcpkg_names(text: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(Value::Object(manifest)) = serde_json::from_str::<Value>(text) else {
        return names;
    };
    let mut blocks: Vec<&Value> = manifest.get("dependencies").into_iter().collect();
    // A feature's dependencies are declared too, even when the feature is off:
    // the manifest is what the project says it may need.
    if let Some(Value::Object(features)) = manifest.get("features") {
        blocks.extend(features.values().filter_map(|f| f.get("dependencies")));
    }
    for block in blocks {
        let Value::Array(entries) = block else {
            continue;
        };
        for entry in entries {
            let raw = match entry {
                Value::String(name) => Some(name.as_str()),
                Value::Object(object) => object.get("name").and_then(Value::as_str),
                _ => None,
            };
            if let Some(name) = raw.and_then(sanitize) {
                names.insert(name);
            }
        }
    }
    names
}

/// The `[requires]`-family sections of a `conanfile.txt`.
///
/// A requirement is `package/version`, so the name is what precedes the slash —
/// which is also why `sanitize` cannot be used here: it rejects a slash.
fn conan_txt_names(text: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            inside = matches!(section.trim(), "requires" | "tool_requires" | "build_requires");
            continue;
        }
        if !inside || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let head = line.split('/').next().unwrap_or(line);
        if let Some(name) = sanitize(head) {
            names.insert(name);
        }
    }
    names
}

/// Requirements from a `conanfile.py`.
///
/// A Python recipe can compute its requirements, so this reads the two forms
/// that are written down: the `requires` class attribute and `self.requires(…)`
/// calls. Both put the requirement in a quoted `package/version` string, and
/// the scan follows an unbalanced bracket onto the next lines so a
/// `requires = (\n "fmt/10.2.1",\n)` list is not truncated.
fn conan_py_names(text: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut open = 0i32;

    for line in text.lines() {
        let mentions = line
            .split(|c: char| !is_name_char(c))
            .any(|word| matches!(word, "requires" | "test_requires" | "tool_requires"));
        if open <= 0 && !mentions {
            continue;
        }
        for quoted in quoted_strings(line) {
            let head = quoted.split('/').next().unwrap_or(&quoted);
            // A requirement always carries a version; a bare word inside a
            // `requires` line is a keyword argument's value, not a package.
            if quoted.contains('/')
                && let Some(name) = sanitize(head)
            {
                names.insert(name);
            }
        }
        open += bracket_balance(line);
        if open < 0 {
            open = 0;
        }
    }
    names
}

/// How much deeper the line leaves the bracket nesting, quotes excluded.
fn bracket_balance(line: &str) -> i32 {
    let mut balance = 0i32;
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        match (quote, ch) {
            (Some(delimiter), c) if c == delimiter => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(ch),
            (None, '(' | '[' | '{') => balance += 1,
            (None, ')' | ']' | '}') => balance -= 1,
            _ => {}
        }
    }
    balance
}

/// Every quoted run in one line, delimiters dropped.
fn quoted_strings(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        match quote {
            Some(delimiter) if ch == delimiter => {
                out.push(std::mem::take(&mut current));
                quote = None;
            }
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None => {}
        }
    }
    out
}

/// `dependency('zlib')` calls in a `meson.build`.
fn meson_names(text: &str) -> HashSet<String> {
    commands(text)
        .into_iter()
        .filter(|command| command.name == "dependency")
        .filter_map(|command| command.args.first().and_then(|arg| sanitize(arg)))
        .collect()
}

/// Every dependency the root declares, from every build file it has.
///
/// The union rather than the first manifest found: a project may well use CMake
/// to build and vcpkg to fetch, and both are declarations.
pub fn dependencies(root: &Path) -> HashSet<String> {
    let mut names: HashSet<String> = HashSet::new();

    let listfile = root.join("CMakeLists.txt");
    if listfile.is_file() {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        cmake_dependencies(&listfile, 0, &mut seen, &mut names);
    }
    if let Ok(text) = fs::read_to_string(root.join("vcpkg.json")) {
        names.extend(vcpkg_names(&text));
    }
    if let Ok(text) = fs::read_to_string(root.join("conanfile.txt")) {
        names.extend(conan_txt_names(&text));
    }
    if let Ok(text) = fs::read_to_string(root.join("conanfile.py")) {
        names.extend(conan_py_names(&text));
    }
    if let Ok(text) = fs::read_to_string(root.join("meson.build")) {
        names.extend(meson_names(&text));
    }
    names
}

/// The declared dependencies of a C or C++ project, sorted.
///
/// One function for both dialects: a `CMakeLists.txt` never says which of the
/// two it builds, and neither does any of the other manifests.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn c_parse_dependencies(root: PathBuf) -> Vec<String> {
    let mut names: Vec<String> = dependencies(&root).into_iter().collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: HashSet<String>) -> Vec<String> {
        let mut out: Vec<String> = names.into_iter().collect();
        out.sort();
        out
    }

    fn names_of(text: &str) -> Vec<String> {
        sorted(cmake_entries(text).0)
    }

    #[test]
    fn reads_a_command_split_over_several_lines() {
        let commands = commands("target_link_libraries(app\n  PRIVATE\n  curl\n)\n");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "target_link_libraries");
        assert_eq!(commands[0].args, ["app", "PRIVATE", "curl"]);
    }

    /// The two spellings of a command name, and the case CMake ignores.
    #[test]
    fn a_name_may_be_separated_from_its_parenthesis() {
        let commands = commands("Project (fixture C)\nfind_package(CURL)\n");
        assert_eq!(commands[0].name, "project");
        assert_eq!(commands[0].args, ["fixture", "C"]);
        assert_eq!(commands[1].name, "find_package");
    }

    /// A `#` inside a quoted argument is data, and a comment ends at the line.
    #[test]
    fn comments_and_quotes_do_not_swallow_each_other() {
        let commands = commands("# find_package(Ghost)\nset(X \"a#b c\")  # trailing\n");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "set");
        assert_eq!(commands[0].args, ["X", "a#b c"]);
    }

    #[test]
    fn nested_parentheses_do_not_end_the_argument_list() {
        let commands = commands("target_link_libraries(app $<IF:$<BOOL:x>,a,b> curl)\n");
        assert_eq!(commands[0].args.len(), 3);
        assert_eq!(commands[0].args[2], "curl");
    }

    /// The fixture's listfile: `engine` is a target of the project and `curl`
    /// is the dependency. Reporting `engine` would put this project's own
    /// library in its dependency list.
    #[test]
    fn a_linked_target_of_this_project_is_not_a_dependency() {
        let text = "\
project(fixture C)
add_library(engine src/engine.c)
add_executable(fixture src/main.c)
target_link_libraries(fixture PRIVATE engine curl)
";
        assert_eq!(names_of(text), ["curl"]);
    }

    #[test]
    fn reads_find_package_fetchcontent_and_pkg_config() {
        let text = "\
find_package(Threads REQUIRED)
pkg_check_modules(CURL REQUIRED libcurl>=7.0)
FetchContent_Declare(fmt GIT_REPOSITORY https://github.com/fmtlib/fmt)
";
        assert_eq!(names_of(text), ["Threads", "fmt", "libcurl"]);
    }

    /// An imported target names its package, which is what a DEPENDENCY node
    /// stands for; a `-l` flag names the library directly.
    #[test]
    fn imported_targets_and_linker_flags_reduce_to_a_package() {
        assert_eq!(sanitize("CURL::libcurl").as_deref(), Some("CURL"));
        assert_eq!(sanitize("Threads::Threads").as_deref(), Some("Threads"));
        assert_eq!(sanitize("-lm").as_deref(), Some("m"));
        assert_eq!(sanitize("libcurl>=7.0").as_deref(), Some("libcurl"));
        assert_eq!(sanitize("  \"fmt\"  ").as_deref(), Some("fmt"));
    }

    /// Anything a configure run would have to evaluate is dropped rather than
    /// reported verbatim.
    #[test]
    fn unevaluated_and_non_package_arguments_are_dropped() {
        for raw in ["${LIBS}", "$<TARGET_FILE:app>", "-Wl,--as-needed", "", "  ", "/usr/lib/libm.a"] {
            assert_eq!(sanitize(raw), None, "{raw:?} was accepted");
        }
    }

    #[test]
    fn a_vendored_subdirectory_is_a_dependency_and_an_internal_one_is_not() {
        assert_eq!(vendored_name("third_party/fmt").as_deref(), Some("fmt"));
        assert_eq!(vendored_name("vendor/googletest/").as_deref(), Some("googletest"));
        assert_eq!(vendored_name("src/engine"), None);
        assert_eq!(vendored_name("tests"), None);
        // The subdirectory is followed either way, which is what the second
        // half of the tuple carries.
        let (names, subdirectories) =
            cmake_entries("add_subdirectory(src)\nadd_subdirectory(external/zlib)\n");
        assert_eq!(sorted(names), ["zlib"]);
        assert_eq!(subdirectories, ["src", "external/zlib"]);
    }

    #[test]
    fn reads_a_vcpkg_manifest_in_both_spellings() {
        let text = r#"{
          "name": "fixture",
          "dependencies": ["fmt", {"name": "boost-asio", "version>=": "1.84"}],
          "features": {"test": {"dependencies": ["catch2"]}}
        }"#;
        assert_eq!(sorted(vcpkg_names(text)), ["boost-asio", "catch2", "fmt"]);
        // Malformed JSON yields nothing rather than failing the analysis.
        assert!(vcpkg_names("{not json").is_empty());
    }

    #[test]
    fn reads_conanfile_txt_sections() {
        let text = "\
[requires]
fmt/10.2.1
# a comment
zlib/1.3

[generators]
CMakeDeps
[tool_requires]
cmake/3.28.1
";
        assert_eq!(sorted(conan_txt_names(text)), ["cmake", "fmt", "zlib"]);
    }

    #[test]
    fn reads_conanfile_py_attributes_and_calls() {
        let text = "\
class Recipe(ConanFile):
    requires = (
        \"fmt/10.2.1\",
        \"zlib/1.3\",
    )
    generators = \"CMakeDeps\"

    def requirements(self):
        self.requires(\"openssl/3.2.0\")
";
        assert_eq!(sorted(conan_py_names(text)), ["fmt", "openssl", "zlib"]);
        // A `requires` line with no versioned string names nothing.
        assert!(conan_py_names("    requires = None\n").is_empty());
    }

    #[test]
    fn reads_meson_dependency_calls() {
        let text = "\
project('fixture', 'c')
zlib = dependency('zlib', required: false)
executable('app', 'main.c', dependencies: [zlib, dependency('libcurl')])
";
        assert_eq!(sorted(meson_names(text)), ["libcurl", "zlib"]);
    }
}
