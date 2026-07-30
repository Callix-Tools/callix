//! The PHP adapter.
//!
//! PHP is the first language callix analyses without a type checker behind it:
//! there is no PHP equivalent of `ty` to link in and no widely installed
//! indexer to shell out to. Resolution is therefore a symbol table built from
//! the sources themselves — see `resolver.rs` for what that can and cannot
//! answer.

mod adapter;
mod boundary;
mod deps;
mod detector;
mod resolver;
mod visitor;

pub use adapter::PhpAdapter;
pub use boundary::extract_php_boundaries;
pub use deps::{classify_php_import, php_parse_dependencies};
pub use detector::{find_php_roots, is_php_project, php_detect_project_name};
pub use resolver::PhpResolver;

use std::cell::RefCell;

use pyo3::prelude::*;
use tree_sitter::{Parser, Tree};

use crate::error::ParseError;

thread_local! {
    /// `LANGUAGE_PHP`, not `LANGUAGE_PHP_ONLY`: a `.php` file starts in HTML
    /// mode and the tags are part of the syntax, so the template-aware
    /// grammar is the one that parses real files.
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .expect("the php grammar is compatible with this tree-sitter");
        parser
    });
}

pub fn parse_tree(source: &[u8]) -> Result<Tree, PyErr> {
    PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .ok_or_else(|| ParseError::new_err("tree-sitter could not parse the source"))
}
