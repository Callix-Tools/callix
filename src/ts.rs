//! Shared helpers on top of tree-sitter: tree walking and position-to-Span
//! conversion.
//!
//! tree-sitter positions are 0-based, Span is 1-based; the conversion lives
//! here and nowhere else.

use std::collections::HashMap;

use tree_sitter::Node as TsNode;

use crate::span::Span;

/// A node's text. Broken UTF-8 is replaced rather than failing the parse.
pub fn text<'a>(node: TsNode<'_>, source: &'a [u8]) -> std::borrow::Cow<'a, str> {
    String::from_utf8_lossy(&source[node.byte_range()])
}

/// Direct children, anonymous ones included (like `node.children` in the
/// Python API).
pub fn children<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// Child lookup walks a cursor instead of collecting a `Vec`: on large trees
/// (TypeScript types) those allocations show up in the profile.
pub fn child_of_type<'tree>(node: TsNode<'tree>, kind: &str) -> Option<TsNode<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).find(|c| c.kind() == kind)
}

pub fn child_of_types<'tree>(node: TsNode<'tree>, kinds: &[&str]) -> Option<TsNode<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|c| kinds.contains(&c.kind()))
}

pub fn has_child_of_type(node: TsNode<'_>, kind: &str) -> bool {
    child_of_type(node, kind).is_some()
}

/// Iterates direct children without an intermediate `Vec`.
pub fn for_each_child<'tree, E>(
    node: TsNode<'tree>,
    mut visit: impl FnMut(TsNode<'tree>) -> Result<(), E>,
) -> Result<(), E> {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return Ok(());
    }
    loop {
        visit(cursor.node())?;
        if !cursor.goto_next_sibling() {
            return Ok(());
        }
    }
}

/// tree-sitter positions (0-based) → Span (1-based).
pub fn span(node: TsNode<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span {
        start_line: start.row as u32 + 1,
        start_col: start.column as u32 + 1,
        end_line: end.row as u32 + 1,
        end_col: end.column as u32 + 1,
    }
}

/// The first `identifier` in pre-order traversal.
///
/// This is how the leading name of a compound type is taken: `list[int]`,
/// `Optional[str]`.
pub fn first_identifier<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    children(node).into_iter().find_map(first_identifier)
}

/// The outermost `call` nodes: we do not descend into a call once found —
/// calls nested in its arguments are handled by the value scanner.
pub fn find_calls<'tree>(node: TsNode<'tree>, out: &mut Vec<TsNode<'tree>>) {
    if node.kind() == "call" {
        out.push(node);
        return;
    }
    for child in children(node) {
        find_calls(child, out);
    }
}

/// Named children — no punctuation or other anonymous nodes.
pub fn named_children<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// Runs a tree-sitter query over a subtree.
///
/// Returns one capture map per match: `@name` → nodes. This lets patterns be
/// written declaratively (the `.scm` query language) instead of as branches
/// in a hand-written visitor.
pub fn run_query<'q, 'tree>(
    query: &'q tree_sitter::Query,
    root: TsNode<'tree>,
    source: &[u8],
) -> Vec<HashMap<&'q str, Vec<TsNode<'tree>>>> {
    use streaming_iterator::StreamingIterator;

    // Capture names live in the query itself, so they are handed out as
    // references — otherwise every match would cost a string allocation.
    let names = query.capture_names();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, root, source);

    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        let mut captures: HashMap<&'q str, Vec<TsNode<'tree>>> = HashMap::new();
        for capture in m.captures {
            captures
                .entry(names[capture.index as usize])
                .or_default()
                .push(capture.node);
        }
        out.push(captures);
    }
    out
}
