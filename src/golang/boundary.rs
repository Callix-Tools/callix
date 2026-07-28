//! Cross-language boundary extraction from Go.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use tree_sitter::{Node as TsNode, Query};

use crate::boundaries::{BoundaryRef, normalize_http_path};
use crate::ts;

const HTTP_VERBS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
const TEMPORAL_EXEC: [&str; 2] = ["executeactivity", "executelocalactivity"];

/// `r.GET("/x", h)` / `http.Get("/x")` — a method call on a receiver.
const Q_SELECTOR_CALL: &str = r#"
(call_expression
  function: (selector_expression
    operand: (identifier) @obj
    field: (field_identifier) @method)
  arguments: (argument_list) @args)
"#;
/// gRPC: protoc generates `New<Service>Client` constructors.
const Q_GRPC_CLIENT_ASSIGN: &str = r#"
(short_var_declaration
  left: (expression_list (identifier) @var)
  right: (expression_list (call_expression
    function: (selector_expression field: (field_identifier) @callee))))
"#;

macro_rules! cached_query {
    ($source:expr) => {{
        static QUERY: OnceLock<Query> = OnceLock::new();
        QUERY.get_or_init(|| {
            Query::new(&tree_sitter_go::LANGUAGE.into(), $source)
                .expect("the query is valid for the go grammar")
        })
    }};
}

struct Extractor<'a> {
    source: &'a [u8],
}

impl<'a> Extractor<'a> {
    fn text(&self, node: TsNode<'_>) -> String {
        ts::text(node, self.source).into_owned()
    }

    fn pos(node: TsNode<'_>) -> (u32, u32) {
        let span = ts::span(node);
        (span.start_line, span.start_col)
    }

    /// The contents of a Go string literal.
    fn string_content(&self, node: TsNode<'_>) -> Option<String> {
        if !matches!(
            node.kind(),
            "interpreted_string_literal" | "raw_string_literal"
        ) {
            return None;
        }
        Some(
            ts::children(node)
                .into_iter()
                .find(|c| {
                    matches!(
                        c.kind(),
                        "interpreted_string_literal_content" | "raw_string_literal_content"
                    )
                })
                .map(|c| self.text(c))
                .unwrap_or_default(),
        )
    }

    /// The first argument, if it is a string literal.
    fn first_string(&self, args: TsNode<'_>) -> Option<String> {
        let first = ts::named_children(args).into_iter().next()?;
        self.string_content(first)
    }

    fn http_ref(role: &str, verb: &str, path: &str, node: TsNode<'_>) -> BoundaryRef {
        let norm = normalize_http_path(path);
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
            confidence: if role == "server" { 1.0 } else { 0.85 },
            detail,
        }
    }

    /// Routes on gin / chi / echo routers (`r.GET(...)`).
    fn http_server(&self, calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(obj), Some(method_node), Some(args)) =
                (cap(caps, "obj"), cap(caps, "method"), cap(caps, "args"))
            else {
                continue;
            };
            // The net/http package is the client side.
            if self.text(obj) == "http" {
                continue;
            }
            let method = self.text(method_node).to_lowercase();
            if !HTTP_VERBS.contains(&method.as_str()) {
                continue;
            }
            let Some(path) = self.first_string(args) else {
                continue;
            };
            refs.push(Self::http_ref("server", &method.to_uppercase(), &path, method_node));
        }
        refs
    }

    /// net/http client calls (`http.Get(url)`).
    fn http_client(&self, calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(obj), Some(method_node), Some(args)) =
                (cap(caps, "obj"), cap(caps, "method"), cap(caps, "args"))
            else {
                continue;
            };
            if self.text(obj) != "http" {
                continue;
            }
            let method = self.text(method_node).to_lowercase();
            if !HTTP_VERBS.contains(&method.as_str()) {
                continue;
            }
            let Some(url) = self.first_string(args) else {
                continue;
            };
            if !url.starts_with('/') && !url.contains("://") {
                continue;
            }
            refs.push(Self::http_ref("client", &method.to_uppercase(), &url, method_node));
        }
        refs
    }

    /// Queue producers (Publish/Produce) and consumers (Subscribe).
    fn queue(&self, calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(method_node), Some(args)) = (cap(caps, "method"), cap(caps, "args")) else {
                continue;
            };
            let Some(role) = queue_role(&self.text(method_node).to_lowercase()) else {
                continue;
            };
            let Some(topic) = self.first_string(args) else {
                continue;
            };
            let (line, col) = Self::pos(method_node);
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

    /// An activity's name from a positional argument.
    fn activity_name(&self, args: TsNode<'_>, index: usize) -> Option<String> {
        let kids = ts::named_children(args);
        let node = *kids.get(index)?;
        match node.kind() {
            "interpreted_string_literal" | "raw_string_literal" => self.string_content(node),
            "identifier" => Some(self.text(node)),
            "selector_expression" => {
                let text = self.text(node);
                Some(text.rsplit('.').next().unwrap_or(&text).to_string())
            }
            _ => None,
        }
    }

    fn temporal_ref(role: &str, name: &str, node: TsNode<'_>) -> BoundaryRef {
        let (line, col) = Self::pos(node);
        let mut detail = IndexMap::new();
        detail.insert("activity".to_string(), name.to_string());
        BoundaryRef {
            mechanism: "temporal".to_string(),
            role: role.to_string(),
            key: name.to_string(),
            line,
            col,
            confidence: 0.9,
            detail,
        }
    }

    /// Temporal: ExecuteActivity (client) and RegisterActivity (server).
    fn temporal(&self, calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(method_node), Some(args)) = (cap(caps, "method"), cap(caps, "args")) else {
                continue;
            };
            let method = self.text(method_node).to_lowercase();
            if TEMPORAL_EXEC.contains(&method.as_str()) {
                // The first argument is the context.
                if let Some(name) = self.activity_name(args, 1) {
                    refs.push(Self::temporal_ref("client", &name, method_node));
                }
            } else if method == "registeractivity"
                && let Some(name) = self.activity_name(args, 0)
            {
                refs.push(Self::temporal_ref("server", &name, method_node));
            }
        }
        refs
    }

    /// Calls on a `New<Service>Client` stub (the client side).
    fn grpc(&self, root: TsNode<'_>, calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut clients: BTreeMap<String, String> = BTreeMap::new();
        for caps in ts::run_query(cached_query!(Q_GRPC_CLIENT_ASSIGN), root, self.source) {
            let (Some(var), Some(callee)) = (cap(&caps, "var"), cap(&caps, "callee")) else {
                continue;
            };
            if let Some(service) = grpc_service(&self.text(callee)) {
                clients.insert(self.text(var), service);
            }
        }

        let mut refs = Vec::new();
        for caps in calls {
            let (Some(obj), Some(method_node)) = (cap(caps, "obj"), cap(caps, "method")) else {
                continue;
            };
            let Some(service) = clients.get(&self.text(obj)) else {
                continue;
            };
            let method = self.text(method_node);
            let (line, col) = Self::pos(method_node);
            let mut detail = IndexMap::new();
            detail.insert("service".to_string(), service.clone());
            detail.insert("method".to_string(), method.clone());
            refs.push(BoundaryRef {
                mechanism: "grpc".to_string(),
                role: "client".to_string(),
                key: format!("{service}/{method}"),
                line,
                col,
                confidence: 0.85,
                detail,
            });
        }
        refs
    }
}

type Caps<'q, 'tree> = std::collections::HashMap<&'q str, Vec<TsNode<'tree>>>;

fn cap<'tree>(caps: &Caps<'_, 'tree>, name: &str) -> Option<TsNode<'tree>> {
    caps.get(name).and_then(|nodes| nodes.first()).copied()
}

fn queue_role(method: &str) -> Option<&'static str> {
    match method {
        // A producer addresses the topic, a consumer serves it.
        "publish" | "produce" => Some("client"),
        "subscribe" => Some("server"),
        _ => None,
    }
}

/// The service name from a `New<Service>Client` constructor.
fn grpc_service(callee: &str) -> Option<String> {
    let stem = callee.strip_prefix("New")?.strip_suffix("Client")?;
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_string())
}

/// Every boundary in one file. The extractor order matches graphlens's.
pub fn extract_boundaries(root: TsNode<'_>, source: &[u8]) -> Vec<BoundaryRef> {
    let extractor = Extractor { source };
    // Every extractor but the gRPC assignments walks the same one query.
    let calls = ts::run_query(cached_query!(Q_SELECTOR_CALL), root, source);

    let mut refs = extractor.http_server(&calls);
    refs.extend(extractor.http_client(&calls));
    refs.extend(extractor.queue(&calls));
    refs.extend(extractor.temporal(&calls));
    refs.extend(extractor.grpc(root, &calls));
    refs
}

/// Boundaries in a single source — the entry point for checks.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn extract_go_boundaries(source: &Bound<'_, PyBytes>) -> PyResult<Vec<BoundaryRef>> {
    let source = source.as_bytes();
    let tree = super::parse_tree(source)?;
    Ok(extract_boundaries(tree.root_node(), source))
}
