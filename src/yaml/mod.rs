//! The YAML adapter: boundaries and service wiring, no symbols.
//!
//! YAML is not a programming language and this adapter does not pretend
//! otherwise — there are no functions to declare and nothing to resolve. What
//! it contributes is the half of a system that code never states: an OpenAPI
//! document lists the routes a service serves, a Compose file lists the
//! services and which of them wait on which, a Kubernetes Ingress lists the
//! paths that reach the cluster.
//!
//! Because boundary IDs derive from the mechanism and the normalized key alone,
//! a route declared in a spec collapses onto the same node as the handler that
//! implements it. That is what makes drift between a specification and its
//! implementation visible in the graph.

mod adapter;
mod boundary;
mod detector;

pub use adapter::YamlAdapter;
pub use boundary::extract_yaml_boundaries;
pub use detector::{find_yaml_roots, is_yaml_project, yaml_detect_project_name};

use std::cell::RefCell;

use pyo3::prelude::*;
use tree_sitter::{Parser, Tree};

use crate::error::ParseError;

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_yaml::LANGUAGE.into())
            .expect("the yaml grammar is compatible with this tree-sitter");
        parser
    });
}

pub fn parse_tree(source: &[u8]) -> Result<Tree, PyErr> {
    PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .ok_or_else(|| ParseError::new_err("tree-sitter could not parse the source"))
}
