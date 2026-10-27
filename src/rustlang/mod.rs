//! The Rust adapter: structure via tree-sitter, symbols via the
//! `rust-analyzer scip` index (see `resolver.rs`).

mod adapter;
mod boundary;
mod deps;
mod macros;
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

/// The `detail` of a `read` / `write` occurrence for a name in expression or pattern
/// position (a const, a struct literal, a pattern path). The visitor sets it and
/// `resolve_pass` reads it: an answer that lands on a local or a parameter is dropped.
pub(crate) const VALUE_DETAIL: &str = "value";

use std::cell::RefCell;

use crate::py::prelude::*;
use tree_sitter::{Parser, Tree};

use crate::error::ParseError;

thread_local! {
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("the rust grammar is compatible with this tree-sitter");
        parser
    });
}

pub fn parse_tree(source: &[u8]) -> Result<Tree, PyErr> {
    PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .ok_or_else(|| ParseError::new_err("tree-sitter could not parse the source"))
}

/// Names the project defines with `macro_rules!`, by a scan of the text: a macro
/// is usable in files read before the one that defines it, so the names are
/// wanted before any file is parsed in earnest.
pub fn defined_macros(source: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(source);
    let mut found = Vec::new();
    let mut rest: &str = &text;
    while let Some(at) = rest.find("macro_rules!") {
        rest = &rest[at + "macro_rules!".len()..];
        let name: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            found.push(name);
        }
    }
    found
}

/// Macros whose arguments are not expressions.
const OPAQUE_MACROS: [&str; 17] = [
    "matches", "cfg", "concat", "stringify", "env", "option_env", "include", "include_str",
    "include_bytes", "file", "line", "column", "module_path", "compile_error", "asm", "global_asm",
    "macro_rules",
];

/// The source with the name of every macro invocation inside a function replaced
/// by blanks, so that `println!("{}", f())` reads as the tuple `("{}", f())` and
/// the calls in a macro's arguments are found where they are written. Nothing
/// moves: the text is as long as before and every line and column is the same,
/// which is what lets the resolver answer for it.
///
/// A macro the project defines is left alone - its arguments are not expressions
/// until the macro says so - and so is one whose arguments are known not to be.
/// None when there is nothing to blank.
pub fn blank_std_macros(source: &[u8], tree: &Tree, project_macros: &std::collections::HashSet<String>) -> Option<Vec<u8>> {
    let mut blanked: Option<Vec<u8>> = None;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
        if node.kind() != "macro_invocation" {
            continue;
        }
        let Some(name) = node.child_by_field_name("macro") else {
            continue;
        };
        let text = String::from_utf8_lossy(&source[name.start_byte()..name.end_byte()]).into_owned();
        let last = text.rsplit("::").next().unwrap_or(&text).trim();
        if project_macros.contains(last) || OPAQUE_MACROS.contains(&last) {
            continue;
        }
        let Some(arguments) = node.children(&mut node.walk()).find(|c| c.kind() == "token_tree") else {
            continue;
        };
        if !matches!(source.get(arguments.start_byte()), Some(b'(' | b'[')) {
            continue;
        }
        // Only inside a function: a macro among the items is not an expression.
        // And in a place an expression goes: `Token![,]` in `parse::<Token![,]>()` is a
        // type, and a pattern or an attribute is no expression either.
        let mut inside = false;
        let mut parent = node.parent();
        while let Some(p) = parent {
            let kind = p.kind();
            if matches!(kind, "block" | "closure_expression") {
                inside = true;
                break;
            }
            if kind.contains("type") || kind.contains("pattern") || kind.contains("attribute") {
                break;
            }
            parent = p.parent();
        }
        if !inside {
            continue;
        }
        // Only arguments that read as expressions on their own: a macro like `quote!`
        // takes tokens that are not, and blanking its name would only confuse the
        // parse of the function around it.
        let mut probe = b"fn __probe() { let _ = ".to_vec();
        probe.extend_from_slice(&source[arguments.start_byte()..arguments.end_byte()]);
        probe.extend_from_slice(b"; }");
        if parse_tree(&probe).map(|t| t.root_node().has_error()).unwrap_or(true) {
            continue;
        }
        let bytes = blanked.get_or_insert_with(|| source.to_vec());
        for byte in &mut bytes[node.start_byte()..arguments.start_byte()] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    blanked
}
