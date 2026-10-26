//! The documentation written next to a definition.
//!
//! A graph that keeps the names and positions of a codebase and drops what its
//! authors wrote about them is half a map. This module reads the two places
//! documentation lives:
//!
//! * the **comment block directly above** a definition — `///` and `//!` in
//!   Rust, `/** ... */` in JSDoc, PHPDoc and Javadoc style, `//` in Go and C,
//!   `#` in Python — adjacent, with no blank line between it and the
//!   definition, and only attributes or decorators allowed in between;
//! * a Python **docstring**, which is the first statement of the body.
//!
//! The result is plain text with the comment markers removed. It lands in
//! `metadata["doc"]` of the node, and the LSIF and SCIP exporters show it as the
//! symbol's hover.

use tree_sitter::Node as TsNode;

use crate::py::prelude::*;
use crate::py::types::{PyDict, PyList};

use crate::ts;

/// Node kinds that are a comment in one grammar or another.
const COMMENT_KINDS: [&str; 3] = ["comment", "line_comment", "block_comment"];

/// Nodes that may sit between a comment and the definition it documents.
const TRANSPARENT: [&str; 4] =
    ["attribute_item", "inner_attribute_item", "attribute_list", "decorator"];

/// A node that wraps the definition it was given, so the comment is above the
/// wrapper: `export class A`, `@deco def f`, `x = 1`, `const x = 1`.
const WRAPPERS: [&str; 10] = [
    "export_statement",
    "decorated_definition",
    "expression_statement",
    "lexical_declaration",
    "variable_declaration",
    "type_declaration",
    "const_declaration",
    "var_declaration",
    "declaration",
    "field_declaration",
];

/// The comment block above `node`, or above the wrapper it sits in.
pub fn leading_comment(node: TsNode<'_>, source: &[u8]) -> Option<String> {
    let mut current = node;
    loop {
        if let Some(found) = comment_block_above(current, source) {
            return Some(found);
        }
        match current.parent() {
            Some(parent) if WRAPPERS.contains(&parent.kind()) => current = parent,
            _ => return None,
        }
    }
}

/// The comments that end on the line before `node` starts, the nearest first
/// when read upward, returned in source order and cleaned.
fn comment_block_above(node: TsNode<'_>, source: &[u8]) -> Option<String> {
    let mut next_row = node.start_position().row;
    let mut found: Vec<TsNode<'_>> = Vec::new();
    let mut previous = node.prev_sibling();
    while let Some(sibling) = previous {
        let kind = sibling.kind();
        // A blank line between two things ends the block.
        if sibling.end_position().row + 1 < next_row {
            break;
        }
        if TRANSPARENT.contains(&kind) {
            next_row = sibling.start_position().row;
        } else if COMMENT_KINDS.contains(&kind) && starts_its_line(sibling, source) {
            found.push(sibling);
            next_row = sibling.start_position().row;
        } else {
            break;
        }
        previous = sibling.prev_sibling();
    }
    if found.is_empty() {
        return None;
    }
    found.reverse();
    let text: Vec<String> = found
        .iter()
        .map(|comment| strip_markers(&ts::text(*comment, source)))
        .collect();
    let joined = text.join("\n");
    let cleaned = joined.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// Whether nothing but whitespace comes before the comment on its line — a
/// trailing comment (`x = 1  # why`) belongs to what it follows, not to what
/// comes next.
fn starts_its_line(comment: TsNode<'_>, source: &[u8]) -> bool {
    let start = comment.start_byte();
    source[..start]
        .iter()
        .rev()
        .take_while(|byte| **byte != b'\n')
        .all(|byte| byte.is_ascii_whitespace())
}

/// One comment, without its delimiters.
fn strip_markers(comment: &str) -> String {
    let comment = comment.trim();
    if let Some(body) = comment.strip_prefix("/*") {
        let body = body.strip_suffix("*/").unwrap_or(body);
        // `/**` and `/*!` open a doc block.
        let body = body.strip_prefix(['*', '!']).unwrap_or(body);
        let lines: Vec<&str> = body
            .lines()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    return line.trim();
                }
                // ` * text` loses its gutter; a line without one keeps its indent.
                match line.trim_start().strip_prefix('*') {
                    Some(rest) => rest.strip_prefix(' ').unwrap_or(rest),
                    None => line.trim_end(),
                }
            })
            .collect();
        return lines.join("\n").trim().to_string();
    }
    if let Some(rest) = comment.strip_prefix("//") {
        // `///` and `//!` are doc comments; `//` is not, and is kept anyway.
        let rest = rest.strip_prefix(['/', '!']).unwrap_or(rest);
        return rest.strip_prefix(' ').unwrap_or(rest).trim_end().to_string();
    }
    if let Some(rest) = comment.strip_prefix('#') {
        return rest.strip_prefix(' ').unwrap_or(rest).trim_end().to_string();
    }
    comment.to_string()
}

/// The docstring of a Python function or class: the string that is the first
/// statement of its body, cleaned the way PEP 257 describes.
pub fn python_docstring(definition: TsNode<'_>, source: &[u8]) -> Option<String> {
    let body = definition.child_by_field_name("body")?;
    let first = ts::named_children(body).into_iter().find(|n| n.kind() != "comment")?;
    string_statement_text(first, source)
}

/// The text of a statement that is a plain string, cleaned as PEP 257 describes.
fn string_statement_text(statement: TsNode<'_>, source: &[u8]) -> Option<String> {
    if statement.kind() != "expression_statement" {
        return None;
    }
    let string = ts::named_children(statement).into_iter().next().filter(|n| n.kind() == "string")?;
    let children = ts::children(string);
    // f-strings and bytes are not documentation.
    if let Some(start) = children.iter().find(|c| c.kind() == "string_start") {
        let prefix = ts::text(*start, source).to_ascii_lowercase();
        if prefix.contains('f') || prefix.contains('b') {
            return None;
        }
    }
    let raw: String = children
        .iter()
        .filter(|c| c.kind() == "string_content")
        .map(|c| ts::text(*c, source).into_owned())
        .collect();
    let cleaned = clean_docstring(&raw);
    (!cleaned.is_empty()).then_some(cleaned)
}

/// The docstring of a Python module: the string that is the first statement of the
/// file.
pub fn python_module_docstring(root: TsNode<'_>, source: &[u8]) -> Option<String> {
    let first = ts::named_children(root).into_iter().find(|n| n.kind() != "comment")?;
    string_statement_text(first, source)
}

/// PEP 257: the first line is stripped, the others lose their common indent,
/// and blank lines at either end go.
fn clean_docstring(raw: &str) -> String {
    let expanded = raw.replace('\t', "        ");
    let mut lines = expanded.lines();
    let first = lines.next().unwrap_or("").trim().to_string();
    let rest: Vec<&str> = lines.collect();
    let indent = rest
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out = vec![first];
    out.extend(rest.iter().map(|line| {
        if line.len() >= indent { line[indent..].trim_end() } else { line.trim() }.to_string()
    }));
    out.join("\n").trim().to_string()
}

/// Definitions a comment inside belongs to rather than to the one around them.
const NESTED_DEFINITIONS: [&str; 10] = [
    "function_definition",
    "class_definition",
    "function_declaration",
    "class_declaration",
    "method_definition",
    "method_declaration",
    "function_item",
    "impl_item",
    "trait_item",
    "abstract_class_declaration",
];

/// The comments written inside a definition: one entry per block of adjacent
/// comment lines, or per comment that follows code on its line, as `(line, text)`
/// with the line 1-based. The comments of a definition nested in it are its own.
///
/// What a docstring or the comment above says is `doc`; these are the rest of what
/// the author wrote about the code, the explanations that sit next to the lines
/// they explain.
pub fn body_comments(definition: TsNode<'_>, source: &[u8]) -> Vec<(u32, String)> {
    let mut found: Vec<TsNode<'_>> = Vec::new();
    let mut stack: Vec<TsNode<'_>> = ts::children(definition);
    while let Some(node) = stack.pop() {
        if COMMENT_KINDS.contains(&node.kind()) {
            found.push(node);
            continue;
        }
        if NESTED_DEFINITIONS.contains(&node.kind()) {
            continue;
        }
        stack.extend(ts::children(node));
    }
    found.sort_by_key(|c| c.start_byte());
    let mut blocks: Vec<(u32, usize, Vec<String>)> = Vec::new();
    for comment in found {
        let row = comment.start_position().row;
        let end_row = comment.end_position().row;
        let text = strip_markers(&ts::text(comment, source));
        if text.is_empty() {
            continue;
        }
        let own_line = starts_its_line(comment, source);
        match blocks.last_mut() {
            // The next line of a block of comments that each have a line to themselves.
            Some((_, last_row, lines)) if own_line && row == *last_row + 1 && lines_are_block(lines) => {
                lines.push(text);
                *last_row = end_row;
            }
            _ => blocks.push((row as u32 + 1, end_row, vec![text])),
        }
        // A comment after code ends its block: the next line is another thought.
        if !own_line && let Some(last) = blocks.last_mut() {
            last.1 = usize::MAX / 2;
        }
    }
    blocks.into_iter().map(|(line, _, lines)| (line, lines.join("\n"))).collect()
}

fn lines_are_block(lines: &[String]) -> bool {
    !lines.is_empty()
}

/// The comment that follows a definition on its last line: `x = 1  # why`.
pub fn trailing_comment(node: TsNode<'_>, source: &[u8]) -> Option<String> {
    let mut current = node;
    loop {
        let end_row = current.end_position().row;
        let mut next = current.next_sibling();
        while let Some(sibling) = next {
            if sibling.start_position().row != end_row {
                break;
            }
            if COMMENT_KINDS.contains(&sibling.kind()) {
                let text = strip_markers(&ts::text(sibling, source));
                return (!text.is_empty()).then_some(text);
            }
            next = sibling.next_sibling();
        }
        match current.parent() {
            Some(parent) if WRAPPERS.contains(&parent.kind()) => current = parent,
            _ => return None,
        }
    }
}

/// Writes `comments` and `trailing_comment` into a node's metadata, when there
/// are any. A definition with a body gets the first; one that is a line gets the
/// second.
pub fn add_comments<'py>(
    py: Python<'py>,
    metadata: &Bound<'py, PyDict>,
    definition: TsNode<'_>,
    source: &[u8],
) -> PyResult<()> {
    const MAX_BLOCKS: usize = 200;
    let comments = body_comments(definition, source);
    if !comments.is_empty() {
        let mut entries = Vec::new();
        for (line, text) in comments.into_iter().take(MAX_BLOCKS) {
            let entry = PyDict::new(py);
            entry.set_item("line", line)?;
            entry.set_item("text", text)?;
            entries.push(entry);
        }
        let list = PyList::new(py, entries)?;
        metadata.set_item("comments", list)?;
    }
    if let Some(text) = trailing_comment(definition, source) {
        metadata.set_item("trailing_comment", text)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_removed() {
        assert_eq!(strip_markers("/// Adds one."), "Adds one.");
        assert_eq!(strip_markers("//! Crate docs"), "Crate docs");
        assert_eq!(strip_markers("// plain"), "plain");
        assert_eq!(strip_markers("# python"), "python");
        assert_eq!(strip_markers("/** One\n * two\n */"), "One\ntwo");
        assert_eq!(strip_markers("/* a\n   b */"), "a\n   b");
    }

    #[test]
    fn a_docstring_loses_its_indent_and_its_outer_blank_lines() {
        let raw = "\n    Summary.\n\n    Detail:\n        indented\n    ";
        assert_eq!(clean_docstring(raw), "Summary.\n\nDetail:\n    indented");
        assert_eq!(clean_docstring("One line."), "One line.");
    }
}
