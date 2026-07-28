//! Извлечение межъязыковых границ из TypeScript.
//!
//! Зеркалит Python-экстракторы: ключи путей проходят через общий
//! `normalize_http_path`, поэтому TS-овый `fetch("/users/1")` и питонячий
//! `@app.get("/users/{id}")` попадают в один и тот же узел BOUNDARY.

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
/// `@Get("/x")` — декоратор метода контроллера в стиле NestJS.
const Q_DECORATOR: &str = r#"
(decorator (call_expression
  function: (identifier) @deco
  arguments: (arguments) @args))
"#;

/// Запросы компилируются под каждую грамматику по одному разу.
fn cached_query(source: &str, tsx: bool) -> &'static Query {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<(String, bool), &'static Query>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut cache = cache.lock().expect("мьютекс кеша запросов не отравлен");
    let key = (source.to_string(), tsx);
    cache.entry(key).or_insert_with(|| {
        let language = if tsx {
            tree_sitter_typescript::LANGUAGE_TSX
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT
        };
        let query = Query::new(&language.into(), source)
            .expect("запрос валиден для грамматики typescript");
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

    /// Строка или шаблонный литерал → шаблон пути; None для прочего.
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

    /// Первый аргумент, если он строка или шаблон.
    fn first_url(&self, args: TsNode<'_>) -> Option<String> {
        let first = ts::named_children(args).into_iter().next()?;
        self.url_template(first)
    }

    /// Содержимое обычного строкового литерала.
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

    /// Метод из `fetch(url, {method: "..."})`.
    ///
    /// По умолчанию GET — но если метод задан литералом, берётся он, иначе
    /// не-GET запрос попал бы в граф как GET.
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

    /// Маршруты Express `app.get(...)` и методы NestJS `@Get(...)`.
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
            // Маршруты Express — абсолютные пути; строка иного вида это
            // `app.get("view engine")`, то есть чтение настройки.
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
            // Путь в декораторе NestJS может быть относительным
            // (`@Get("users")`), но пустая строка маршрутом не бывает.
            let Some(url) = self.first_url(*args).filter(|u| !u.is_empty()) else {
                continue;
            };
            refs.push(Self::http_ref("server", &method.to_uppercase(), &url, *deco));
        }
        refs
    }

    /// Клиентские вызовы `fetch(...)` и `axios.get(...)`.
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

    /// Продюсеры (publish/produce/emit) и консьюмеры очередей.
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
        // Продюсер обращается к топику, консьюмер его обслуживает.
        "publish" | "produce" | "emit" => Some("client"),
        "subscribe" => Some("server"),
        _ => None,
    }
}

/// Все границы одного разобранного файла. Порядок экстракторов тот же,
/// что в graphlens, — от него зависит порядок рёбер в графе.
pub fn extract_boundaries(root: TsNode<'_>, source: &[u8], tsx: bool) -> Vec<BoundaryRef> {
    let extractor = Extractor { source, tsx };
    let mut refs = extractor.http_server(root);
    refs.extend(extractor.http_client(root));
    refs.extend(extractor.queue(root));
    refs
}

/// Границы в одном исходнике — точка входа для проверок.
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
