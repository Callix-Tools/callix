//! Cross-language boundaries declared in YAML.
//!
//! YAML holds no functions to call, which is exactly why it matters here: an
//! OpenAPI document states the routes a service serves without any code saying
//! so, and a Kubernetes Ingress states the paths that reach it. Because a
//! boundary's ID comes from the mechanism and the normalized key alone, a
//! route written in a spec lands on the same node as the handler that
//! implements it and the client that calls it — which is what makes "does the
//! spec match the code" answerable.

use indexmap::IndexMap;
use tree_sitter::Node as TsNode;

use crate::boundaries::{BoundaryRef, normalize_http_path};
use crate::ts;

const HTTP_VERBS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

/// What a document turned out to be.
///
/// YAML has no manifest to consult, so the flavour is read off the top-level
/// keys — the same way a person recognizes the file at a glance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavour {
    /// `openapi:` or `swagger:` at the top level.
    OpenApi,
    /// `services:` at the top level — Compose.
    Compose,
    /// `apiVersion:` and `kind:` — a Kubernetes manifest.
    Kubernetes,
    Unknown,
}

/// The text of a scalar, unquoted.
fn scalar(node: TsNode<'_>, source: &[u8]) -> String {
    ts::text(node, source)
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string()
}

/// Every `key: value` pair of a mapping, in document order.
///
/// A YAML value arrives wrapped in `block_node` or `flow_node`; the wrapper is
/// unhelpful to callers, so it is peeled here once.
pub fn pairs<'tree>(node: TsNode<'tree>, source: &[u8]) -> Vec<(String, TsNode<'tree>)> {
    let mut out = Vec::new();
    let Some(mapping) = unwrap_node(node).filter(|n| n.kind() == "block_mapping") else {
        return out;
    };
    for pair in ts::children(mapping) {
        if pair.kind() != "block_mapping_pair" {
            continue;
        }
        let (Some(key), Some(value)) = (
            pair.child_by_field_name("key"),
            pair.child_by_field_name("value"),
        ) else {
            continue;
        };
        out.push((scalar(key, source), value));
    }
    out
}

/// Peels `block_node` / `flow_node` wrappers down to the content.
fn unwrap_node<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    let mut current = node;
    loop {
        match current.kind() {
            "block_node" | "flow_node" | "document" | "stream" => {
                current = ts::named_children(current).into_iter().next()?;
            }
            _ => return Some(current),
        }
    }
}

/// Items of a block sequence.
pub fn items<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let Some(sequence) = unwrap_node(node).filter(|n| n.kind() == "block_sequence") else {
        return Vec::new();
    };
    ts::children(sequence)
        .into_iter()
        .filter(|c| c.kind() == "block_sequence_item")
        .filter_map(|item| ts::named_children(item).into_iter().next())
        .collect()
}

/// The document's root mapping, past the stream and document wrappers.
pub fn root_mapping<'tree>(root: TsNode<'tree>, source: &[u8]) -> Vec<(String, TsNode<'tree>)> {
    for document in ts::named_children(root) {
        let found = pairs(document, source);
        if !found.is_empty() {
            return found;
        }
    }
    pairs(root, source)
}

/// Recognizes the document from its top-level keys.
pub fn flavour(root: TsNode<'_>, source: &[u8]) -> Flavour {
    let keys: Vec<String> = root_mapping(root, source)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let has = |name: &str| keys.iter().any(|k| k == name);

    if has("openapi") || has("swagger") {
        Flavour::OpenApi
    } else if has("services") {
        Flavour::Compose
    } else if has("apiVersion") && has("kind") {
        Flavour::Kubernetes
    } else {
        Flavour::Unknown
    }
}

fn http_ref(role: &str, verb: &str, path: &str, node: TsNode<'_>, source: &str) -> BoundaryRef {
    let norm = normalize_http_path(path);
    let span = ts::span(node);
    let mut detail = IndexMap::new();
    detail.insert("method".to_string(), verb.to_string());
    detail.insert("path".to_string(), norm.clone());
    detail.insert("source".to_string(), source.to_string());
    BoundaryRef {
        mechanism: "http".to_string(),
        role: role.to_string(),
        key: format!("{verb} {norm}"),
        line: span.start_line,
        col: span.start_col,
        // A specification states the contract outright; nothing is inferred.
        confidence: 1.0,
        detail,
    }
}

/// `paths:` of an OpenAPI document — every path crossed with every verb.
fn openapi(root: TsNode<'_>, source: &[u8]) -> Vec<BoundaryRef> {
    let mut refs = Vec::new();
    for (key, value) in root_mapping(root, source) {
        if key != "paths" {
            continue;
        }
        for (route, operations) in pairs(value, source) {
            for (verb, _) in pairs(operations, source) {
                if HTTP_VERBS.contains(&verb.to_lowercase().as_str()) {
                    refs.push(http_ref(
                        "server",
                        &verb.to_uppercase(),
                        &route,
                        value,
                        "openapi",
                    ));
                }
            }
        }
    }
    refs
}

/// `spec.rules[].http.paths[].path` of a Kubernetes Ingress.
///
/// An Ingress states which paths reach the cluster, and says nothing about the
/// method, so the key records it as ANY — the same shape `net/http`'s own
/// router produces.
fn kubernetes(root: TsNode<'_>, source: &[u8]) -> Vec<BoundaryRef> {
    let top = root_mapping(root, source);
    let kind = top
        .iter()
        .find(|(k, _)| k == "kind")
        .map(|(_, v)| scalar(*v, source))
        .unwrap_or_default();
    if kind != "Ingress" {
        return Vec::new();
    }

    let mut refs = Vec::new();
    for (key, spec) in &top {
        if key != "spec" {
            continue;
        }
        for (spec_key, rules) in pairs(*spec, source) {
            if spec_key != "rules" {
                continue;
            }
            for rule in items(rules) {
                for (rule_key, http) in pairs(rule, source) {
                    if rule_key != "http" {
                        continue;
                    }
                    for (http_key, paths) in pairs(http, source) {
                        if http_key != "paths" {
                            continue;
                        }
                        for entry in items(paths) {
                            for (entry_key, path) in pairs(entry, source) {
                                if entry_key == "path" {
                                    refs.push(http_ref(
                                        "server",
                                        "ANY",
                                        &scalar(path, source),
                                        path,
                                        "kubernetes-ingress",
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    refs
}

/// Every boundary a YAML document declares.
pub fn extract_boundaries(root: TsNode<'_>, source: &[u8]) -> Vec<BoundaryRef> {
    match flavour(root, source) {
        Flavour::OpenApi => openapi(root, source),
        Flavour::Kubernetes => kubernetes(root, source),
        // Compose declares services and their wiring, which become nodes
        // rather than ports; see `adapter.rs`.
        Flavour::Compose | Flavour::Unknown => Vec::new(),
    }
}

/// Boundaries in a single source — the entry point for checks.
#[pyo3_stub_gen::derive::gen_stub_pyfunction]
#[pyo3::pyfunction]
pub fn extract_yaml_boundaries(
    source: &pyo3::Bound<'_, pyo3::types::PyBytes>,
) -> pyo3::PyResult<Vec<BoundaryRef>> {
    use pyo3::prelude::*;
    let source = source.as_bytes();
    let tree = super::parse_tree(source)?;
    Ok(extract_boundaries(tree.root_node(), source))
}
