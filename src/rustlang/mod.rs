//! Rust-адаптер: структура через tree-sitter, символы — через индекс
//! `rust-analyzer scip` (см. `resolver.rs`).

mod adapter;
mod boundary;
mod deps;
mod resolver;
mod scip;
mod visitor;

pub use adapter::RustAdapter;
pub use boundary::extract_rust_boundaries;
pub use deps::{
    classify_rust_import, find_rust_roots, is_rust_project, rust_detect_project_name,
    rust_parse_dependencies, rust_read_crate_name,
};
pub use resolver::RustResolver;

use std::cell::RefCell;

use pyo3::prelude::*;
use tree_sitter::{Parser, Tree};

use crate::error::ParseError;

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("грамматика rust совместима с этой версией tree-sitter");
        parser
    });
}

pub fn parse_tree(source: &[u8]) -> Result<Tree, PyErr> {
    PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .ok_or_else(|| ParseError::new_err("tree-sitter не смог разобрать исходник"))
}
