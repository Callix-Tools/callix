//! The C and C++ adapters: two grammars, one implementation.
//!
//! C and C++ are two adapters on the Python side — a repository may be a C
//! project, a C++ project, or legitimately both — and one body of code here.
//! Everything that is hard about the family is shared: the header model
//! (`headers.rs`), the descent through declarator chains and `#if` arms
//! (`visitor.rs`), and linkage detection. C's node set is a subset of C++'s for
//! every construct that matters, so a second visitor would be the first one
//! with the `namespace` and `class` arms deleted — and would drift.
//!
//! [`Dialect`] is what the shared code branches on. It picks the grammar, it
//! decides whether a parameter-type list joins a qualified name (C++ overloads;
//! C has none), and it decides whether a namespace-scope `const` variable is
//! file-local (it is in C++, not in C).
//!
//! **MODULE means something different again here.** CLAUDE.md notes the node
//! scheme differs per language; this family adds two more shapes:
//!
//! - **C has no module concept at all.** No namespaces, no packages, no
//!   headers-as-modules — a translation unit is the only unit, and it is
//!   already the FILE node. The MODULE is therefore the DIRECTORY holding the
//!   file, which is the only honest grouping the language offers and the same
//!   choice the Go adapter makes with a package directory.
//! - **C++'s MODULE is the NAMESPACE.** A namespace is a real scope with a
//!   name that qualified names are built from, so it groups declarations the
//!   way a Python package does. A file may declare several, and a namespace may
//!   span several files, which is why a MODULE here is not the file's parent
//!   the way it is in C.
//!
//! Neither shape is a C++20 `module`: nothing in this adapter reads
//! `import std;`, and a project using named modules is analysed through its
//! `#include`s like any other.

mod adapter;
mod boundary;
mod deps;
mod detector;
mod headers;
mod resolver;
mod visitor;

pub use adapter::{CAdapter, CppAdapter};
pub use boundary::{extract_c_boundaries, extract_cpp_boundaries};
pub use deps::c_parse_dependencies;
pub use detector::{
    c_detect_project_name, cpp_detect_project_name, find_c_roots, find_cpp_roots,
    is_c_project, is_cpp_project,
};
pub use resolver::CFamilyResolver;

use std::cell::RefCell;

use pyo3::prelude::*;
use tree_sitter::{Parser, Tree};

use crate::error::ParseError;

/// Which of the two grammars reads a file.
///
/// Not a pyclass: it never crosses into Python. The adapters are two separate
/// classes there, each pinning its own dialect, which is what lets a mixed
/// repository be analysed twice — once per dialect — and merged.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Dialect {
    C,
    Cpp,
}

impl Dialect {
    /// The value `Adapter.language()` reports and metadata carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::C => "c",
            Self::Cpp => "cpp",
        }
    }
}

thread_local! {
    static C_PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .expect("the c grammar is compatible with this tree-sitter");
        parser
    });

    /// A second parser rather than one that switches language per call:
    /// `set_language` resets the parser, and an ambiguous `.h` is parsed with
    /// both grammars in a row (see `headers::probe_dialect`) — that would make
    /// every probe pay for two language switches.
    static CPP_PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("the c++ grammar is compatible with this tree-sitter");
        parser
    });
}

pub fn parse_tree(source: &[u8], dialect: Dialect) -> Result<Tree, PyErr> {
    let parsed = match dialect {
        Dialect::C => C_PARSER.with(|p| p.borrow_mut().parse(source, None)),
        Dialect::Cpp => CPP_PARSER.with(|p| p.borrow_mut().parse(source, None)),
    };
    parsed.ok_or_else(|| ParseError::new_err("tree-sitter could not parse the source"))
}
