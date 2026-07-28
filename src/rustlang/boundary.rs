//! Извлечение межъязыковых границ из Rust.
//!
//! Сервер: axum `.route("/x", get(handler))` (глагол — обёртка обработчика)
//! и атрибуты actix/rocket `#[get("/x")]`. Клиент: reqwest
//! `client.get("/x")`. Ключи проходят через общий `normalize_http_path`.

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
/// axum `route(path, handler)`: обёртка обработчика — второй аргумент.
const HANDLER_ARG_INDEX: usize = 1;

/// Конструкторы клиента gRPC у tonic.
const GRPC_CTORS: [&str; 2] = ["new", "connect"];
const GRPC_CLIENT_SUFFIX: &str = "Client";

/// `recv.method(args)` — axum `.route(...)` / reqwest `client.get(...)`.
const Q_METHOD_CALL: &str = r#"
(call_expression
  function: (field_expression
    value: (_) @recv
    field: (field_identifier) @method)
  arguments: (arguments) @args)
"#;
/// Атрибут-макрос `#[get("/x")]`.
const Q_ATTRIBUTE: &str = r#"
(attribute_item
  (attribute (identifier) @attr arguments: (token_tree) @args))
"#;
/// `let client = ...;` — из значения достаётся конструктор gRPC-стаба.
const Q_LET_IDENT: &str = r#"
(let_declaration
  pattern: (identifier) @var
  value: (_) @value)
"#;

macro_rules! cached_query {
    ($source:expr) => {{
        static QUERY: OnceLock<Query> = OnceLock::new();
        QUERY.get_or_init(|| {
            Query::new(&tree_sitter_rust::LANGUAGE.into(), $source)
                .expect("запрос валиден для грамматики rust")
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

    /// Содержимое строкового литерала Rust; None — если это не литерал.
    fn string_content(&self, node: TsNode<'_>) -> Option<String> {
        if node.kind() != "string_literal" {
            return None;
        }
        Some(
            ts::child_of_type(node, "string_content")
                .map(|c| self.text(c))
                .unwrap_or_default(),
        )
    }

    /// Первый аргумент, если он строковый литерал.
    fn first_string(&self, args: TsNode<'_>) -> Option<String> {
        let first = ts::named_children(args).into_iter().next()?;
        self.string_content(first)
    }

    /// Для axum `route(path, get(h))` — глагол во втором аргументе.
    fn handler_verb(&self, args: TsNode<'_>) -> Option<String> {
        let positional = ts::named_children(args);
        let handler = *positional.get(HANDLER_ARG_INDEX)?;
        if handler.kind() != "call_expression" {
            return None;
        }
        let verb = self
            .text(handler.child_by_field_name("function")?)
            .to_lowercase();
        HTTP_VERBS.contains(&verb.as_str()).then_some(verb)
    }

    /// Первый строковый литерал внутри token tree атрибута.
    fn string_in_tree(&self, tree: TsNode<'_>) -> Option<String> {
        ts::children(tree)
            .into_iter()
            .find(|c| c.kind() == "string_literal")
            .and_then(|c| self.string_content(c))
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

    /// Маршруты axum `.route(...)` и атрибуты actix/rocket `#[get(...)]`.
    fn http_server(&self, root: TsNode<'_>, calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(method_node), Some(args)) = (cap(caps, "method"), cap(caps, "args")) else {
                continue;
            };
            if self.text(method_node) != "route" {
                continue;
            }
            let (Some(path), Some(verb)) = (self.first_string(args), self.handler_verb(args))
            else {
                continue;
            };
            refs.push(Self::http_ref("server", &verb.to_uppercase(), &path, method_node));
        }
        for caps in ts::run_query(cached_query!(Q_ATTRIBUTE), root, self.source) {
            let (Some(attr), Some(args)) = (cap(&caps, "attr"), cap(&caps, "args")) else {
                continue;
            };
            let verb = self.text(attr).to_lowercase();
            if !HTTP_VERBS.contains(&verb.as_str()) {
                continue;
            }
            let Some(path) = self.string_in_tree(args) else {
                continue;
            };
            refs.push(Self::http_ref("server", &verb.to_uppercase(), &path, attr));
        }
        refs
    }

    /// Клиентские вызовы reqwest (`client.get("/x")`).
    fn http_client(&self, calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(method_node), Some(args)) = (cap(caps, "method"), cap(caps, "args")) else {
                continue;
            };
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

    /// Продюсеры (publish/produce) и консьюмеры (subscribe) очередей.
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

    /// Сервис из конструктора `<Service>Client::new` внутри значения.
    fn grpc_service_from_value(&self, value: TsNode<'_>) -> Option<String> {
        let mut stack = vec![value];
        while let Some(node) = stack.pop() {
            if node.kind() == "scoped_identifier"
                && let Some(name) = node.child_by_field_name("name")
                && GRPC_CTORS.contains(&self.text(name).as_str())
            {
                let path = self.text(node.child_by_field_name("path")?);
                let last = path.rsplit("::").next().unwrap_or(&path);
                if let Some(service) = last.strip_suffix(GRPC_CLIENT_SUFFIX)
                    && !service.is_empty()
                {
                    return Some(service.to_string());
                }
            }
            stack.extend(ts::children(node));
        }
        None
    }

    /// Вызовы RPC на сгенерированном стабе tonic (клиентская сторона).
    fn grpc(&self, root: TsNode<'_>, calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut clients: BTreeMap<String, String> = BTreeMap::new();
        for caps in ts::run_query(cached_query!(Q_LET_IDENT), root, self.source) {
            let (Some(var), Some(value)) = (cap(&caps, "var"), cap(&caps, "value")) else {
                continue;
            };
            if let Some(service) = self.grpc_service_from_value(value) {
                clients.insert(self.text(var), service);
            }
        }

        let mut refs = Vec::new();
        for caps in calls {
            let (Some(recv), Some(method_node), Some(args)) =
                (cap(caps, "recv"), cap(caps, "method"), cap(caps, "args"))
            else {
                continue;
            };
            let Some(service) = clients.get(&self.text(recv)) else {
                continue;
            };
            let method = self.text(method_node);
            // Метод-HTTP-глагол со строкой-URL в аргументе — это вызов в
            // стиле reqwest (его разбирает http_client), а не RPC: у RPC
            // аргумент всегда сообщение-запрос, а не URL.
            if HTTP_VERBS.contains(&method.to_lowercase().as_str())
                && let Some(url) = self.first_string(args)
                && (url.starts_with('/') || url.contains("://"))
            {
                continue;
            }
            let (line, col) = Self::pos(method_node);
            let pascal = snake_to_pascal(&method);
            let mut detail = IndexMap::new();
            detail.insert("service".to_string(), service.clone());
            detail.insert("method".to_string(), pascal.clone());
            refs.push(BoundaryRef {
                mechanism: "grpc".to_string(),
                role: "client".to_string(),
                key: format!("{service}/{pascal}"),
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
        // Продюсер обращается к топику, консьюмер его обслуживает.
        "publish" | "produce" => Some("client"),
        "subscribe" => Some("server"),
        _ => None,
    }
}

/// `get_user` → `GetUser`, чтобы ключи Rust сходились с Go и Python.
fn snake_to_pascal(name: &str) -> String {
    name.split('_')
        .map(|part| match part.chars().next() {
            Some(first) => format!("{}{}", first.to_uppercase(), &part[first.len_utf8()..]),
            None => String::new(),
        })
        .collect()
}

/// Все границы одного файла. Порядок экстракторов — как в graphlens.
pub fn extract_boundaries(root: TsNode<'_>, source: &[u8]) -> Vec<BoundaryRef> {
    let extractor = Extractor { source };
    // Три экстрактора из четырёх ходят по одному и тому же запросу.
    let calls = ts::run_query(cached_query!(Q_METHOD_CALL), root, source);

    let mut refs = extractor.http_server(root, &calls);
    refs.extend(extractor.http_client(&calls));
    refs.extend(extractor.queue(&calls));
    refs.extend(extractor.grpc(root, &calls));
    refs
}

/// Границы в одном исходнике — точка входа для проверок.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn extract_rust_boundaries(source: &Bound<'_, PyBytes>) -> PyResult<Vec<BoundaryRef>> {
    let source = source.as_bytes();
    let tree = super::parse_tree(source)?;
    Ok(extract_boundaries(tree.root_node(), source))
}
