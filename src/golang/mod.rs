//! Go-адаптер. Пока здесь резолвер символов поверх `go/packages`,
//! слинкованного в модуль (см. `go/bridge_go.go` и `build.rs`).

mod adapter;
mod boundary;
mod deps;
mod resolver;
mod visitor;

pub use deps::{
    classify_go_import, find_go_roots, go_detect_project_name, go_parse_dependencies,
    go_read_module_path, is_go_project,
};
pub use adapter::GoAdapter;
pub use boundary::extract_go_boundaries;
pub use resolver::GoResolver;
pub use visitor::GoExtractor;

use std::cell::RefCell;

use pyo3::prelude::*;
use tree_sitter::{Parser, Tree};

use crate::error::ParseError;

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .expect("грамматика go совместима с этой версией tree-sitter");
        parser
    });
}

pub fn parse_tree(source: &[u8]) -> Result<Tree, PyErr> {
    PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .ok_or_else(|| ParseError::new_err("tree-sitter не смог разобрать исходник"))
}
