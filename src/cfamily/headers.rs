//! The header model: one FILE node per header, one node per entity.
//!
//! A header is textually included into many translation units, which breaks the
//! rule every other adapter leans on — that a FILE node CONTAINS the
//! declarations found in it. Reparsing a header once per includer would declare
//! the same function once per `.c` that sees it, and `engine_build` would end up
//! as fifteen nodes nobody can query. The model that avoids it:
//!
//! 1. A header is parsed on its own, exactly once, and gets one FILE node.
//! 2. An `#include` becomes an IMPORT node, with RESOLVES_TO to the included
//!    file's FILE node when the path lands inside the project.
//! 3. A name with EXTERNAL linkage carries NO file component, so the prototype
//!    in `engine.h` and the definition in `engine.c` collapse into ONE node.
//!    That is not a merge accident — they are one entity, and a graph that
//!    reported two would answer "who calls engine_build" with half the callers.
//! 4. A name with INTERNAL linkage is prefixed `{file}::`. Two files may each
//!    define `static void log_line(void)`; they are different functions, and a
//!    shared qualified name would fuse them into one.
//! 5. Because one node then has several declaring sites, the site the node is
//!    BUILT from has to be chosen before any node is written — that is what
//!    [`DeclLedger`] is for.
//! 6. A `.h` is claimed by exactly ONE adapter, decided by parse probe. If both
//!    claimed it, the merged graph would hold two different nodes under one id
//!    and `Graph::merge` would raise: `Node.__eq__` compares `file_path` and
//!    `span`, and two grammars do not agree on the span of a declaration one of
//!    them cannot read.

use std::collections::HashSet;
use std::path::Path;

use indexmap::IndexMap;
use pyo3::PyErr;
use tree_sitter::Node as TsNode;

use crate::node::NodeKind;
use crate::span::Span;
use crate::ts;

use super::detector::{
    AMBIGUOUS_HEADER_EXTENSIONS, C_SOURCE_EXTENSIONS, CPP_HEADER_EXTENSIONS,
    CPP_SOURCE_EXTENSIONS, has_extension,
};
use super::Dialect;

/// What joins a name to the scope around it.
///
/// C has no scope operator of its own, but the family needs one separator, and
/// `::` is already what the internal-linkage prefix (`{file}::{name}`) uses.
pub const SCOPE_SEPARATOR: &str = "::";

/// What joins a member to its owner: a struct field, a parameter.
///
/// Deliberately not `::`, and deliberately the same as Go's and Rust's
/// adapters: a field is reached through a value, not through a scope.
pub const MEMBER_SEPARATOR: &str = ".";

/// Whether a name is visible outside the file that declares it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Linkage {
    Internal,
    External,
}

impl Linkage {
    /// The value stored in `Node.metadata["linkage"]`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::External => "external",
        }
    }
}

/// The qualified name of a name declared in a scope.
pub fn qualify(file_rel: &str, scope: &[String], name: &str, linkage: Linkage) -> String {
    let prefix = qualify_scope(file_rel, scope, linkage);
    if prefix.is_empty() {
        return name.to_string();
    }
    format!("{prefix}{SCOPE_SEPARATOR}{name}")
}

/// The qualified name of the scope itself — a namespace, or a class holding
/// members.
///
/// The file component appears only for internal linkage, and that single `if`
/// is the whole header model: without it a `static` helper is one node for the
/// whole project, and with it applied unconditionally a prototype and its
/// definition never meet.
pub fn qualify_scope(file_rel: &str, scope: &[String], linkage: Linkage) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(scope.len() + 1);
    if linkage == Linkage::Internal {
        parts.push(file_rel);
    }
    parts.extend(scope.iter().map(String::as_str));
    parts.join(SCOPE_SEPARATOR)
}

/// Where one declaring site of a shared name sits.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeclSite {
    pub file_rel: String,
    pub span: Span,
    pub name_span: Span,
    /// Whether this site is the definition: a function with a body, a record
    /// with a member list, a variable that is not merely `extern`.
    pub defines: bool,
}

/// A node's identity while the ledger fills.
///
/// The kind is part of it because `node_id` mixes the kind in, and because C
/// keeps tags in their own namespace: `struct stat` and `stat()` coexist in
/// every libc and must not share an entry.
pub type DeclKey = (NodeKind, String);

struct DeclEntry {
    /// The site the node is built from.
    owner: DeclSite,
    /// Every file that declared the name, in the order the files were walked.
    declared_in: Vec<String>,
}

/// Which of a name's declaring sites the node is built from.
///
/// Filled by a survey pass over every file of the root, read by the emit pass.
/// Splitting the two is not an optimization: a prototype in a header and a
/// definition in a source file are one node, and whichever file the adapter
/// happened to read first would otherwise decide the node's span. That is a
/// determinism bug, not a cosmetic one.
#[derive(Default)]
pub struct DeclLedger {
    entries: IndexMap<DeclKey, DeclEntry>,
}

impl DeclLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one declaring site.
    ///
    /// The definition wins. Between two definitions, or two declarations, the
    /// first site wins — and "first" is well defined only because the adapter
    /// walks files in `roots::sort_paths` order.
    pub fn record(&mut self, key: DeclKey, site: DeclSite) {
        if let Some(entry) = self.entries.get_mut(&key) {
            if !entry.declared_in.iter().any(|file| file == &site.file_rel) {
                entry.declared_in.push(site.file_rel.clone());
            }
            if site.defines && !entry.owner.defines {
                entry.owner = site;
            }
            return;
        }
        self.entries.insert(
            key,
            DeclEntry { declared_in: vec![site.file_rel.clone()], owner: site },
        );
    }

    /// Whether this exact site is the one the node is built from.
    ///
    /// The span is part of the test rather than just the file: a prototype and
    /// its definition commonly sit in the SAME file, and only one of the two may
    /// write the node.
    pub fn owns(&self, key: &DeclKey, site: &DeclSite) -> bool {
        self.entries.get(key).is_some_and(|entry| {
            entry.owner.file_rel == site.file_rel && entry.owner.span == site.span
        })
    }

    /// Every declaring file other than the one the node was built from — the
    /// value of `metadata["declared_in"]`.
    ///
    /// The owner's own file is left out because it is already the node's
    /// `file_path`.
    pub fn declared_elsewhere(&self, key: &DeclKey) -> Vec<String> {
        self.entries
            .get(key)
            .map(|entry| {
                entry
                    .declared_in
                    .iter()
                    .filter(|file| file.as_str() != entry.owner.file_rel.as_str())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Where a `<...>` include is looked for inside the project.
///
/// The conventional layout only. A real include path lives in the build system,
/// and parsing CMake or make well enough to find it is a compiler's job rather
/// than a graph's; an include that misses falls through to an EXTERNAL_SYMBOL,
/// so being wrong here costs no edge. The empty entry is the root itself and is
/// last, so `include/engine.h` wins over `engine.h` when both exist.
pub const CONVENTIONAL_INCLUDE_DIRS: [&str; 4] = ["include", "inc", "src", ""];

/// The conventional include directories that this root actually has.
pub fn include_search_dirs(root: &Path) -> Vec<String> {
    CONVENTIONAL_INCLUDE_DIRS
        .iter()
        .filter(|dir| dir.is_empty() || root.join(dir).is_dir())
        .map(|dir| (*dir).to_string())
        .collect()
}

/// The project-relative path an `#include` names, or None when it leaves the
/// project.
///
/// A quoted include is looked up next to the including file first, which is what
/// every compiler does and what makes a sibling header resolve with no
/// configuration at all. Only paths the adapter already collected are accepted:
/// resolving against the filesystem would happily bind `/usr/include/stdio.h`,
/// which has no FILE node to point at.
pub fn resolve_include(
    raw: &str,
    quoted: bool,
    includer_rel: &str,
    search_dirs: &[String],
    known: &HashSet<String>,
) -> Option<String> {
    if quoted {
        let dir = Path::new(includer_rel)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default();
        let sibling = join_relative(&dir, raw);
        if known.contains(&sibling) {
            return Some(sibling);
        }
    }
    search_dirs
        .iter()
        .map(|dir| join_relative(dir, raw))
        .find(|candidate| known.contains(candidate))
}

/// `dir` joined with `raw`, with `.` and `..` folded away.
///
/// Folded lexically and never through the filesystem: `canonicalize` resolves
/// symlinks, and a path that differs between two checkouts of the same project
/// cannot appear in a qualified name.
fn join_relative(dir: &str, raw: &str) -> String {
    let combined = if dir.is_empty() {
        raw.to_string()
    } else {
        format!("{dir}/{raw}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in combined.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// The dialect an extension settles on its own; None for the ambiguous `.h`.
pub fn dialect_from_extension(path: &Path) -> Option<Dialect> {
    if has_extension(path, &C_SOURCE_EXTENSIONS) {
        return Some(Dialect::C);
    }
    if has_extension(path, &CPP_SOURCE_EXTENSIONS) || has_extension(path, &CPP_HEADER_EXTENSIONS) {
        return Some(Dialect::Cpp);
    }
    None
}

/// Whether the extension is the one that names no dialect.
pub fn is_ambiguous_header(path: &Path) -> bool {
    has_extension(path, &AMBIGUOUS_HEADER_EXTENSIONS)
}

/// The dialect that reads an ambiguous header best.
///
/// C is tried first and kept when it parses cleanly. The order matters: the C++
/// grammar accepts nearly every C header too, so probing C++ first would settle
/// nothing.
///
/// When C stumbles, the grammar leaving fewer ERROR and MISSING nodes wins, and
/// a tie goes to C. Counting the nodes rather than trusting `has_error` is what
/// makes the fallback useful in both directions: a C++ header full of templates
/// parses as C with a handful of errors, while a C header using `class` or `new`
/// as an identifier breaks the C++ grammar instead.
pub fn probe_dialect(source: &[u8]) -> Result<Dialect, PyErr> {
    let as_c = super::parse_tree(source, Dialect::C)?;
    if !as_c.root_node().has_error() {
        return Ok(Dialect::C);
    }
    let as_cpp = super::parse_tree(source, Dialect::Cpp)?;
    if error_score(as_cpp.root_node()) < error_score(as_c.root_node()) {
        return Ok(Dialect::Cpp);
    }
    Ok(Dialect::C)
}

/// ERROR and MISSING nodes in the tree.
///
/// A subtree with no error inside is skipped whole — `has_error` is a flag
/// tree-sitter already maintains per node, and without the prune this walks
/// every node of every header twice.
fn error_score(root: TsNode<'_>) -> usize {
    let mut score = 0usize;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            score += 1;
        }
        if node.has_error() {
            stack.extend(ts::children(node));
        }
    }
    score
}

/// Whether this dialect's adapter should analyse the file.
///
/// Exactly one adapter may claim a given `.h`; see the module header for what
/// happens to `Graph::merge` if two do.
pub fn claims(dialect: Dialect, path: &Path, source: &[u8]) -> Result<bool, PyErr> {
    if let Some(owner) = dialect_from_extension(path) {
        return Ok(owner == dialect);
    }
    if !is_ambiguous_header(path) {
        return Ok(false);
    }
    Ok(probe_dialect(source)? == dialect)
}
