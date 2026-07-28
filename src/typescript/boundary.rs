//! Cross-language boundary extraction from TypeScript.
//!
//! Mirrors the Python extractors: path keys go through the shared
//! `normalize_http_path`, so TypeScript's `fetch("/users/1")` and Python's
//! `@app.get("/users/{id}")` land on the same BOUNDARY node.

use std::collections::HashMap;
use std::sync::OnceLock;

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use tree_sitter::{Node as TsNode, Query};

use crate::boundaries::{BoundaryRef, normalize_http_path};
use crate::ts;

const HTTP_VERBS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
const SERVER_OBJECTS: [&str; 3] = ["app", "router", "server"];
const CLIENT_OBJECTS: [&str; 3] = ["axios", "http", "https"];

/// `app.get("/x", handler)` / `axios.post("/x", body)`.
const Q_MEMBER_CALL: &str = r#"
(call_expression
  function: (member_expression
    object: (identifier) @obj
    property: (property_identifier) @method)
  arguments: (arguments) @args)
"#;
/// `fetch("/x")`.
const Q_FETCH: &str = r#"
(call_expression
  function: (identifier) @fn
  arguments: (arguments) @args)
"#;
/// `@Get("/x")` — a NestJS-style controller method decorator.
const Q_DECORATOR: &str = r#"
(decorator (call_expression
  function: (identifier) @deco
  arguments: (arguments) @args))
"#;

/// Queries are compiled once per grammar.
fn cached_query(source: &str, tsx: bool) -> &'static Query {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<(String, bool), &'static Query>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut cache = cache.lock().expect("the query cache mutex is not poisoned");
    let key = (source.to_string(), tsx);
    cache.entry(key).or_insert_with(|| {
        let language = if tsx {
            tree_sitter_typescript::LANGUAGE_TSX
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT
        };
        let query = Query::new(&language.into(), source)
            .expect("the query is valid for the typescript grammar");
        Box::leak(Box::new(query))
    })
}

struct Extractor<'a> {
    source: &'a [u8],
    tsx: bool,
}

impl<'a> Extractor<'a> {
    fn text(&self, node: TsNode<'_>) -> String {
        ts::text(node, self.source).into_owned()
    }

    fn pos(node: TsNode<'_>) -> (u32, u32) {
        let span = ts::span(node);
        (span.start_line, span.start_col)
    }

    /// A string or template literal → a path template; None otherwise.
    fn url_template(&self, node: TsNode<'_>) -> Option<String> {
        match node.kind() {
            "string" => Some(
                ts::children(node)
                    .into_iter()
                    .find(|c| c.kind() == "string_fragment")
                    .map(|c| self.text(c))
                    .unwrap_or_default(),
            ),
            "template_string" => {
                let mut parts = Vec::new();
                for child in ts::children(node) {
                    match child.kind() {
                        "string_fragment" => parts.push(self.text(child)),
                        "template_substitution" => parts.push("{}".to_string()),
                        _ => {}
                    }
                }
                Some(parts.concat())
            }
            _ => None,
        }
    }

    /// The first argument, if it is a string or a template.
    fn first_url(&self, args: TsNode<'_>) -> Option<String> {
        let first = ts::named_children(args).into_iter().next()?;
        self.url_template(first)
    }

    /// The contents of a plain string literal.
    fn string_literal(&self, node: TsNode<'_>) -> Option<String> {
        if node.kind() != "string" {
            return None;
        }
        Some(
            ts::children(node)
                .into_iter()
                .find(|c| c.kind() == "string_fragment")
                .map(|c| self.text(c))
                .unwrap_or_default(),
        )
    }

    /// The method from `fetch(url, {method: "..."})`.
    ///
    /// GET by default — but a literal method is taken when present, or a
    /// non-GET request would enter the graph as a GET.
    fn fetch_method(&self, args: TsNode<'_>) -> String {
        let kids = ts::named_children(args);
        if kids.len() < 2 || kids[1].kind() != "object" {
            return "GET".to_string();
        }
        for pair in ts::named_children(kids[1]) {
            if pair.kind() != "pair" {
                continue;
            }
            let Some(key) = pair.child_by_field_name("key") else {
                continue;
            };
            let key_name = if key.kind() == "string" {
                self.string_literal(key).unwrap_or_default()
            } else {
                self.text(key)
            };
            if key_name != "method" {
                continue;
            }
            if let Some(value) = pair.child_by_field_name("value")
                && let Some(method) = self.string_literal(value)
                && !method.is_empty()
            {
                return method.to_uppercase();
            }
        }
        "GET".to_string()
    }

    fn is_http_path(url: &str) -> bool {
        url.starts_with('/') || url.contains("://")
    }

    fn http_ref(role: &str, verb: &str, url: &str, node: TsNode<'_>) -> BoundaryRef {
        let norm = normalize_http_path(url);
        let (line, col) = Self::pos(node);
        let mut detail = IndexMap::new();
        detail.insert("method".to_string(), verb.to_string());
        detail.insert("path".to_string(), norm.clone());
        BoundaryRef {
            mechanism: "http".to_string(),
            role: role.to_string(),
            key: format!("{verb} {norm}"),
            line,
            col,
            confidence: if role == "server" { 1.0 } else { 0.9 },
            detail,
        }
    }

    /// Express routes `app.get(...)` and NestJS methods `@Get(...)`.
    fn http_server(&self, root: TsNode<'_>) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();

        for caps in ts::run_query(cached_query(Q_MEMBER_CALL, self.tsx), root, self.source) {
            let (Some(obj), Some(method_node), Some(args)) = (
                caps.get("obj").and_then(|v| v.first()),
                caps.get("method").and_then(|v| v.first()),
                caps.get("args").and_then(|v| v.first()),
            ) else {
                continue;
            };
            if !SERVER_OBJECTS.contains(&self.text(*obj).as_str()) {
                continue;
            }
            let method = self.text(*method_node).to_lowercase();
            if !HTTP_VERBS.contains(&method.as_str()) {
                continue;
            }
            // Express routes are absolute paths; any other string is
            // `app.get("view engine")`, i.e. reading a setting.
            let Some(url) = self.first_url(*args) else {
                continue;
            };
            if !Self::is_http_path(&url) {
                continue;
            }
            refs.push(Self::http_ref(
                "server",
                &method.to_uppercase(),
                &url,
                *method_node,
            ));
        }

        for caps in ts::run_query(cached_query(Q_DECORATOR, self.tsx), root, self.source) {
            let (Some(deco), Some(args)) = (
                caps.get("deco").and_then(|v| v.first()),
                caps.get("args").and_then(|v| v.first()),
            ) else {
                continue;
            };
            let method = self.text(*deco).to_lowercase();
            if !HTTP_VERBS.contains(&method.as_str()) {
                continue;
            }
            // A NestJS decorator's path may be relative (`@Get("users")`),
            // but an empty string is never a route.
            let Some(url) = self.first_url(*args).filter(|u| !u.is_empty()) else {
                continue;
            };
            refs.push(Self::http_ref("server", &method.to_uppercase(), &url, *deco));
        }
        refs
    }

    /// Client calls `fetch(...)` and `axios.get(...)`.
    fn http_client(&self, root: TsNode<'_>) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();

        for caps in ts::run_query(cached_query(Q_FETCH, self.tsx), root, self.source) {
            let (Some(function), Some(args)) = (
                caps.get("fn").and_then(|v| v.first()),
                caps.get("args").and_then(|v| v.first()),
            ) else {
                continue;
            };
            if self.text(*function) != "fetch" {
                continue;
            }
            let Some(url) = self.first_url(*args) else {
                continue;
            };
            if !Self::is_http_path(&url) {
                continue;
            }
            let method = self.fetch_method(*args);
            refs.push(Self::http_ref("client", &method, &url, *function));
        }

        for caps in ts::run_query(cached_query(Q_MEMBER_CALL, self.tsx), root, self.source) {
            let (Some(obj), Some(method_node), Some(args)) = (
                caps.get("obj").and_then(|v| v.first()),
                caps.get("method").and_then(|v| v.first()),
                caps.get("args").and_then(|v| v.first()),
            ) else {
                continue;
            };
            if !CLIENT_OBJECTS.contains(&self.text(*obj).as_str()) {
                continue;
            }
            let method = self.text(*method_node).to_lowercase();
            if !HTTP_VERBS.contains(&method.as_str()) {
                continue;
            }
            let Some(url) = self.first_url(*args) else {
                continue;
            };
            if !Self::is_http_path(&url) {
                continue;
            }
            refs.push(Self::http_ref(
                "client",
                &method.to_uppercase(),
                &url,
                *method_node,
            ));
        }
        refs
    }

    /// Queue producers (publish/produce/emit) and consumers.
    fn queue(&self, root: TsNode<'_>) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in ts::run_query(cached_query(Q_MEMBER_CALL, self.tsx), root, self.source) {
            let (Some(method_node), Some(args)) = (
                caps.get("method").and_then(|v| v.first()),
                caps.get("args").and_then(|v| v.first()),
            ) else {
                continue;
            };
            let Some(role) = queue_role(&self.text(*method_node).to_lowercase()) else {
                continue;
            };
            let Some(topic) = self.first_url(*args) else {
                continue;
            };
            let (line, col) = Self::pos(*method_node);
            let mut detail = IndexMap::new();
            detail.insert("topic".to_string(), topic.clone());
            refs.push(BoundaryRef {
                mechanism: "queue".to_string(),
                role: role.to_string(),
                key: topic,
                line,
                col,
                confidence: if role == "server" { 0.75 } else { 0.7 },
                detail,
            });
        }
        refs
    }
}

fn queue_role(method: &str) -> Option<&'static str> {
    match method {
        // A producer addresses the topic, a consumer serves it.
        "publish" | "produce" | "emit" => Some("client"),
        "subscribe" => Some("server"),
        _ => None,
    }
}

/// Every boundary in one parsed file. The extractor order matches
/// graphlens's, because edge order in the graph depends on it.
pub fn extract_boundaries(root: TsNode<'_>, source: &[u8], tsx: bool) -> Vec<BoundaryRef> {
    let extractor = Extractor { source, tsx };
    let mut refs = extractor.http_server(root);
    refs.extend(extractor.http_client(root));
    refs.extend(extractor.queue(root));
    refs
}

/// Boundaries in a single source — the entry point for checks.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (source, tsx = false))]
pub fn extract_typescript_boundaries(
    source: &Bound<'_, PyBytes>,
    tsx: bool,
) -> PyResult<Vec<BoundaryRef>> {
    let source = source.as_bytes();
    let tree = super::parse_tree(source, tsx)?;
    Ok(extract_boundaries(tree.root_node(), source, tsx))
}
