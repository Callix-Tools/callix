//! The names a TypeScript type mentions.
//!
//! `a.T`, `Array<Shape>`, `A | B`, `typeof v`, `Shape["k"]` and `{ [k in keyof S]: T }` name
//! types, not only their first identifier. The walk is pure: it returns the nodes and says
//! whether each is a type or a value (a namespace prefix or a `typeof` operand). A type
//! parameter, a mapped-type key and an `infer` name are never returned: they are not nodes,
//! so an edge to one could only be a per-site `unlocated` external symbol.

use tree_sitter::Node as TsNode;

use crate::ts;

/// How a name found in a type is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypeUse {
    /// The name of a type: `T` in `a.T`, `Shape` in `Array<Shape>`.
    Type,
    /// A value the type reaches through: the `a` of `a.T`, the operand of `typeof v`.
    Value,
}

type Found<'t> = Vec<(TsNode<'t>, TypeUse)>;

/// Every type name in `node`, in source order, without duplicates.
pub(crate) fn type_uses<'t>(node: TsNode<'t>, source: &[u8]) -> Found<'t> {
    let mut out = Vec::new();
    walk(node, source, &mut out);
    out
}

/// The identifiers of a value chain (`a`, `a.b`, `a.b.c`, `f<T>`) in source order. Anything
/// that is not a plain chain (a call, a subscript) yields nothing.
pub(crate) fn value_chain<'t>(node: TsNode<'t>, source: &[u8]) -> Vec<TsNode<'t>> {
    let mut out = Vec::new();
    values(node, source, &mut out);
    out.into_iter().map(|(n, _)| n).collect()
}

/// The aliased type of a `type X = ...` declaration.
pub(crate) fn alias_value<'t>(alias: TsNode<'t>) -> Option<TsNode<'t>> {
    if let Some(value) = alias.child_by_field_name("value") {
        return Some(value);
    }
    let mut past_eq = false;
    for child in ts::children(alias) {
        if !child.is_named() && child.kind() == "=" {
            past_eq = true;
            continue;
        }
        if past_eq && child.is_named() {
            return Some(child);
        }
    }
    None
}

fn push<'t>(out: &mut Found<'t>, node: TsNode<'t>, how: TypeUse) {
    let at = node.start_byte();
    if !out.iter().any(|(n, _)| n.start_byte() == at) {
        out.push((node, how));
    }
}

fn walk<'t>(node: TsNode<'t>, source: &[u8], out: &mut Found<'t>) {
    match node.kind() {
        "type_identifier" => {
            if !is_bound(node, source) {
                push(out, node, TypeUse::Type);
            }
        }
        "nested_type_identifier" => {
            if let Some(module) = node.child_by_field_name("module") {
                values(module, source, out);
            }
            if let Some(name) = node.child_by_field_name("name") {
                push(out, name, TypeUse::Type);
            }
        }
        "generic_type" => {
            if let Some(name) = node.child_by_field_name("name") {
                walk(name, source, out);
            }
            if let Some(args) = node.child_by_field_name("type_arguments") {
                walk(args, source, out);
            }
        }
        // `typeof v`: the operand is a value.
        "type_query" => {
            for child in ts::named_children(node) {
                values(child, source, out);
            }
        }
        // `x is T`: `x` is a parameter name.
        "type_predicate" => {
            if let Some(ty) = node.child_by_field_name("type") {
                walk(ty, source, out);
            }
        }
        // `[k in keyof S as A]`: `k` is declared here.
        "mapped_type_clause" => {
            for field in ["type", "alias"] {
                if let Some(child) = node.child_by_field_name(field) {
                    walk(child, source, out);
                }
            }
        }
        // `infer U`: `U` is declared here; an `extends` constraint is a type.
        "infer_type" => {
            let mut first = true;
            for child in ts::named_children(node) {
                if first && child.kind() == "type_identifier" {
                    first = false;
                    continue;
                }
                first = false;
                walk(child, source, out);
            }
        }
        "predefined_type" | "this_type" | "literal_type" => {}
        _ => {
            for child in ts::named_children(node) {
                walk(child, source, out);
            }
        }
    }
}

/// A value chain reached from a type.
fn values<'t>(node: TsNode<'t>, source: &[u8], out: &mut Found<'t>) {
    match node.kind() {
        "identifier" | "property_identifier" => push(out, node, TypeUse::Value),
        "member_expression" | "nested_identifier" => {
            for child in ts::named_children(node) {
                values(child, source, out);
            }
        }
        // `typeof f<T>`
        "instantiation_expression" => {
            if let Some(function) = node.child_by_field_name("function") {
                values(function, source, out);
            }
            if let Some(args) = node.child_by_field_name("type_arguments") {
                walk(args, source, out);
            }
        }
        _ => {}
    }
}

/// Whether `node` names a type parameter, a mapped-type key or an `infer` name in scope.
fn is_bound(node: TsNode<'_>, source: &[u8]) -> bool {
    let name = ts::text(node, source).into_owned();
    let name = name.as_str();
    let mut current = node.parent();
    while let Some(ancestor) = current {
        match ancestor.kind() {
            "index_signature" => {
                if declares_mapped_key(ancestor, name, source) {
                    return true;
                }
            }
            "conditional_type" => {
                if let Some(right) = ancestor.child_by_field_name("right")
                    && declares_infer(right, name, source)
                {
                    return true;
                }
            }
            _ => {}
        }
        if declares_type_parameter(ancestor, name, source) {
            return true;
        }
        current = ancestor.parent();
    }
    false
}

fn declares_type_parameter(owner: TsNode<'_>, name: &str, source: &[u8]) -> bool {
    ts::children(owner)
        .into_iter()
        .filter(|c| c.kind() == "type_parameters")
        .any(|params| {
            ts::children(params).into_iter().any(|p| {
                p.kind() == "type_parameter"
                    && p.child_by_field_name("name")
                        .is_some_and(|n| ts::text(n, source).as_ref() == name)
            })
        })
}

fn declares_mapped_key(index_signature: TsNode<'_>, name: &str, source: &[u8]) -> bool {
    ts::named_children(index_signature).into_iter().any(|c| {
        c.kind() == "mapped_type_clause"
            && c.child_by_field_name("name")
                .is_some_and(|n| ts::text(n, source).as_ref() == name)
    })
}

fn declares_infer(node: TsNode<'_>, name: &str, source: &[u8]) -> bool {
    let named = ts::named_children(node);
    if node.kind() == "infer_type"
        && named
            .first()
            .is_some_and(|c| c.kind() == "type_identifier" && ts::text(*c, source).as_ref() == name)
    {
        return true;
    }
    named.into_iter().any(|c| declares_infer(c, name, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uses(src: &str) -> Vec<(String, TypeUse)> {
        let source = format!("type Z = {src};");
        let tree = match crate::typescript::parse_tree(source.as_bytes(), false) {
            Ok(tree) => tree,
            Err(_) => panic!("the source must parse"),
        };
        let alias = tree.root_node().child(0).expect("an alias");
        let value = alias_value(alias).expect("an aliased type");
        type_uses(value, source.as_bytes())
            .into_iter()
            .map(|(n, u)| (ts::text(n, source.as_bytes()).into_owned(), u))
            .collect()
    }

    fn t(name: &str) -> (String, TypeUse) {
        (name.to_string(), TypeUse::Type)
    }

    fn v(name: &str) -> (String, TypeUse) {
        (name.to_string(), TypeUse::Value)
    }

    #[test]
    fn nested_name_is_a_type_and_its_prefix_a_value() {
        assert_eq!(uses("a.T"), vec![v("a"), t("T")]);
        assert_eq!(uses("a.b.T"), vec![v("a"), v("b"), t("T")]);
    }

    #[test]
    fn generic_head_and_arguments() {
        assert_eq!(uses("Array<Shape>"), vec![t("Array"), t("Shape")]);
    }

    #[test]
    fn union_and_intersection_members() {
        assert_eq!(uses("A | B & C"), vec![t("A"), t("B"), t("C")]);
    }

    #[test]
    fn typeof_operand_is_a_value() {
        assert_eq!(uses("typeof v"), vec![v("v")]);
    }

    #[test]
    fn indexed_access_walks_the_object_type() {
        assert_eq!(uses("Shape[\"a\"]"), vec![t("Shape")]);
    }

    #[test]
    fn mapped_key_is_not_a_use() {
        assert_eq!(uses("{ [k in keyof S]: a.T }"), vec![t("S"), v("a"), t("T")]);
    }

    #[test]
    fn infer_name_is_not_a_use() {
        assert_eq!(uses("T extends infer U ? U : never"), vec![t("T")]);
    }

    #[test]
    fn function_type_parameters_are_not_uses() {
        assert!(uses("<X>(x: X) => X").is_empty());
    }

    #[test]
    fn predefined_types_yield_nothing() {
        assert!(uses("number").is_empty());
    }
}
