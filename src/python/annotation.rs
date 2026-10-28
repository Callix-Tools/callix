//! The names a Python type annotation mentions.
//!
//! `list[Item]`, `Optional["Item"]`, `dict[str, Item] | None`, `a.b.C` and
//! `Callable[[A, B], R]` all name types, not only their first identifier. The walk
//! works on spans because the names inside a quoted forward reference come from a
//! second parse whose nodes cannot outlive the call; positions inside the string
//! are shifted by where the string content starts in the real source.

use tree_sitter::{Node as TsNode, Point};

use crate::span::Span;

/// One name found in an annotation.
///
/// `qualifier` is true for the `a` and `b` of `a.b.C`: they are read as values
/// (modules or namespaces) and are not the type. The type is the trailing `C`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TypeName {
    pub span: Span,
    pub qualifier: bool,
}

/// Where the parsed text starts in the real source (0-based).
#[derive(Clone, Copy)]
struct Offset {
    row: usize,
    col: usize,
}

/// Every type name in `node`, in source order, without duplicates.
///
/// `node` is a `type` node or an expression in a type position (the right-hand
/// side of `X: TypeAlias = ...`). Calls, keyword arguments, literals, the
/// arguments of `Literal[...]` and the metadata of `Annotated[...]` yield nothing.
pub(crate) fn type_names(node: TsNode<'_>, source: &[u8]) -> Vec<TypeName> {
    let mut out = Vec::new();
    walk(node, source, Offset { row: 0, col: 0 }, &mut out);
    out
}

fn absolute(point: Point, off: Offset) -> (usize, usize) {
    if point.row == 0 {
        (point.row + off.row, point.column + off.col)
    } else {
        (point.row + off.row, point.column)
    }
}

fn push(out: &mut Vec<TypeName>, node: TsNode<'_>, off: Offset, qualifier: bool) {
    let (start_row, start_col) = absolute(node.start_position(), off);
    let (end_row, end_col) = absolute(node.end_position(), off);
    let span = Span {
        start_line: start_row as u32 + 1,
        start_col: start_col as u32 + 1,
        end_line: end_row as u32 + 1,
        end_col: end_col as u32 + 1,
    };
    if !out.iter().any(|n| n.span == span) {
        out.push(TypeName { span, qualifier });
    }
}

fn named<'t>(node: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut cursor = node.walk();
    let children: Vec<TsNode<'t>> = node.named_children(&mut cursor).collect();
    children
}

/// The identifiers of a plain name chain (`a.b.C`), in order. Anything that is
/// not part of such a chain (a call, a subscript) is skipped.
fn chain<'t>(node: TsNode<'t>, out: &mut Vec<TsNode<'t>>) {
    match node.kind() {
        "identifier" => out.push(node),
        "attribute" | "member_type" => {
            for child in named(node) {
                chain(child, out);
            }
        }
        _ => {}
    }
}

/// The last identifier of a head: `Literal` for `Literal` and `typing.Literal`.
fn head_name<'s>(node: TsNode<'_>, source: &'s [u8]) -> Option<&'s str> {
    let mut ids = Vec::new();
    chain(node, &mut ids);
    ids.last().and_then(|n| n.utf8_text(source).ok())
}

/// The arguments after the head of a `generic_type` or `subscript`. The
/// `type_parameter` of a `generic_type` is flattened into its members.
fn arguments<'t>(rest: &[TsNode<'t>]) -> Vec<TsNode<'t>> {
    let mut args = Vec::new();
    for node in rest {
        if node.kind() == "type_parameter" {
            args.extend(named(*node));
        } else {
            args.push(*node);
        }
    }
    args
}

fn walk(node: TsNode<'_>, source: &[u8], off: Offset, out: &mut Vec<TypeName>) {
    match node.kind() {
        "type" | "parenthesized_expression" | "splat_type" | "union_type" | "list" | "tuple" => {
            for child in named(node) {
                walk(child, source, off, out);
            }
        }
        "identifier" => push(out, node, off, false),
        "attribute" | "member_type" => {
            let mut ids = Vec::new();
            chain(node, &mut ids);
            let count = ids.len();
            for (index, id) in ids.into_iter().enumerate() {
                push(out, id, off, index + 1 < count);
            }
        }
        "generic_type" | "subscript" => {
            let children = named(node);
            let Some((head, rest)) = children.split_first() else {
                return;
            };
            walk(*head, source, off, out);
            let args = arguments(rest);
            match head_name(*head, source) {
                // The arguments are values, not types.
                Some("Literal") => {}
                // Only the first argument is a type; the rest is metadata.
                Some("Annotated") => {
                    if let Some(first) = args.first() {
                        walk(*first, source, off, out);
                    }
                }
                _ => {
                    for arg in args {
                        walk(arg, source, off, out);
                    }
                }
            }
        }
        "binary_operator" => {
            let is_union = node
                .child_by_field_name("operator")
                .is_some_and(|op| op.kind() == "|");
            if is_union {
                for child in named(node) {
                    walk(child, source, off, out);
                }
            }
        }
        "string" => walk_string(node, source, off, out),
        _ => {}
    }
}

fn has_special(node: TsNode<'_>) -> bool {
    if matches!(node.kind(), "escape_sequence" | "interpolation") {
        return true;
    }
    named(node).into_iter().any(has_special)
}

/// A plain string annotation (`"Item"`, `"list['Item']"`): its content is parsed
/// as one expression and walked with positions shifted into the real source.
/// Prefixes f/b/u, escapes, interpolations, several content pieces, a parse
/// error or anything but a single expression make it be ignored.
fn walk_string(node: TsNode<'_>, source: &[u8], off: Offset, out: &mut Vec<TypeName>) {
    let mut cursor = node.walk();
    let children: Vec<TsNode<'_>> = node.children(&mut cursor).collect();
    let mut content = None;
    let mut pieces = 0;
    for child in &children {
        match child.kind() {
            "string_start" => {
                let prefix = child.utf8_text(source).unwrap_or("f");
                if prefix.chars().any(|c| matches!(c, 'f' | 'F' | 'b' | 'B' | 'u' | 'U')) {
                    return;
                }
            }
            "string_content" => {
                if has_special(*child) {
                    return;
                }
                pieces += 1;
                content = Some(*child);
            }
            "string_end" => {}
            _ => return,
        }
    }
    let (1, Some(content)) = (pieces, content) else {
        return;
    };
    let text = &source[content.start_byte()..content.end_byte()];
    let Ok(tree) = crate::python::parse_tree(text) else {
        return;
    };
    let root = tree.root_node();
    if root.has_error() {
        return;
    }
    let statements = named(root);
    let [statement] = statements.as_slice() else {
        return;
    };
    if statement.kind() != "expression_statement" {
        return;
    }
    let expressions = named(*statement);
    let [expression] = expressions.as_slice() else {
        return;
    };
    let (row, col) = absolute(content.start_position(), off);
    walk(*expression, text, Offset { row, col }, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_type<'t>(node: TsNode<'t>) -> Option<TsNode<'t>> {
        if node.kind() == "type" {
            return Some(node);
        }
        named(node).into_iter().find_map(find_type)
    }

    /// (text, line, col, qualifier) of every name in `x: <annotation>`.
    fn names(annotation: &str) -> Vec<(String, u32, u32, bool)> {
        let source = format!("x: {annotation}\n");
        let tree = crate::python::parse_tree(source.as_bytes()).unwrap_or_else(|_| panic!("parse"));
        let ty = find_type(tree.root_node()).expect("a type node");
        type_names(ty, source.as_bytes())
            .into_iter()
            .map(|n| {
                let line = source.lines().nth(n.span.start_line as usize - 1).unwrap();
                let text = &line[n.span.start_col as usize - 1..n.span.end_col as usize - 1];
                (text.to_string(), n.span.start_line, n.span.start_col, n.qualifier)
            })
            .collect()
    }

    fn texts(annotation: &str) -> Vec<String> {
        names(annotation).into_iter().map(|n| n.0).collect()
    }

    #[test]
    fn generic_type_names_head_and_argument() {
        assert_eq!(
            names("list[Item]"),
            vec![("list".into(), 1, 4, false), ("Item".into(), 1, 9, false)]
        );
    }

    #[test]
    fn union_names_both_sides() {
        assert_eq!(
            names("A | B"),
            vec![("A".into(), 1, 4, false), ("B".into(), 1, 8, false)]
        );
    }

    #[test]
    fn callable_lists_and_tuples_are_walked() {
        assert_eq!(texts("Callable[[A, B], R]"), vec!["Callable", "A", "B", "R"]);
        assert_eq!(texts("tuple[A, ...]"), vec!["tuple", "A"]);
    }

    #[test]
    fn the_trailing_identifier_of_an_attribute_is_the_type() {
        assert_eq!(
            names("a.b.C"),
            vec![
                ("a".into(), 1, 4, true),
                ("b".into(), 1, 6, true),
                ("C".into(), 1, 8, false)
            ]
        );
    }

    #[test]
    fn a_nested_string_composes_offsets() {
        assert_eq!(
            names("\"list['Item']\""),
            vec![("list".into(), 1, 5, false), ("Item".into(), 1, 11, false)]
        );
    }

    #[test]
    fn a_triple_quoted_string_offsets_rows() {
        assert_eq!(
            names("\"\"\"\nlist[Item]\n\"\"\""),
            vec![("list".into(), 2, 1, false), ("Item".into(), 2, 6, false)]
        );
    }

    #[test]
    fn escapes_prefixes_and_unparseable_strings_yield_nothing() {
        assert!(names(r#""Ite\x6dm""#).is_empty());
        assert!(names(r#"f"{X}""#).is_empty());
        assert!(names(r#""not valid(""#).is_empty());
    }

    #[test]
    fn literal_arguments_and_annotated_metadata_are_not_types() {
        assert_eq!(texts(r#"Literal["Item"]"#), vec!["Literal"]);
        assert_eq!(texts("Annotated[Item, Depends(f)]"), vec!["Annotated", "Item"]);
    }
}
