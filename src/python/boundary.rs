//! Извлечение межъязыковых границ из Python-исходника.
//!
//! Каждый экстрактор распознаёт один *механизм* границы и возвращает
//! языконезависимые `BoundaryRef` (server — предоставляет, client —
//! потребляет). Адаптер превращает их в узлы BOUNDARY и рёбра
//! EXPOSES / CONSUMES, так что Python-сервер и, скажем, TypeScript-клиент
//! встречаются в одном узле графа.
//!
//! Шаблоны описаны декларативными tree-sitter запросами, а не ветками
//! ручного визитора.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use indexmap::IndexMap;

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use tree_sitter::{Node as TsNode, Query};

use crate::boundaries::{BoundaryRef, normalize_http_path};
use crate::ts;

const HTTP_VERBS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
const TEMPORAL_EXEC: [&str; 4] = [
    "execute_activity",
    "execute_local_activity",
    "start_activity",
    "execute_activity_method",
];

/// Вызов-декоратор: `@app.get("/x")` / `@activity.defn(name=...)`.
const Q_DECORATOR_CALL: &str = r#"
(decorated_definition
  (decorator (call
    function: (attribute) @deco
    arguments: (argument_list) @args))
  definition: (function_definition name: (identifier) @func))
"#;
/// Голый декоратор-атрибут: `@activity.defn`.
const Q_DECORATOR_BARE: &str = r#"
(decorated_definition
  (decorator (attribute) @deco)
  definition: (function_definition name: (identifier) @func))
"#;
/// Вызов атрибута: `requests.get("/x")` / `workflow.execute_activity(a)`.
const Q_ATTR_CALL: &str = r#"
(call
  function: (attribute attribute: (identifier) @method)
  arguments: (argument_list) @args) @call
"#;
// gRPC: protoc порождает базы `<Service>Servicer` и стабы `<Service>Stub`.
const Q_SERVICER_CLASS: &str = r#"
(class_definition
  superclasses: (argument_list) @bases
  body: (block) @body)
"#;
const Q_STUB_ASSIGN: &str = r#"
(assignment
  left: (identifier) @var
  right: (call function: (_) @callee))
"#;
const Q_OBJ_CALL: &str = r#"
(call function: (attribute
  object: (identifier) @obj
  attribute: (identifier) @method))
"#;

/// Компиляция запроса дороже прогона, поэтому она разовая на процесс.
macro_rules! cached_query {
    ($source:expr) => {{
        static QUERY: OnceLock<Query> = OnceLock::new();
        QUERY.get_or_init(|| {
            Query::new(&tree_sitter_python::LANGUAGE.into(), $source)
                .expect("запрос валиден для грамматики python")
        })
    }};
}

/// Карта захватов одного совпадения: `@имя` → узлы.
type Caps<'q, 'tree> = HashMap<&'q str, Vec<TsNode<'tree>>>;

/// Первый узел под захватом.
fn cap<'tree>(caps: &Caps<'_, 'tree>, name: &str) -> Option<TsNode<'tree>> {
    caps.get(name).and_then(|nodes| nodes.first()).copied()
}

struct Extractor<'a> {
    source: &'a [u8],
}

impl<'a> Extractor<'a> {
    fn text(&self, node: TsNode<'_>) -> String {
        ts::text(node, self.source).into_owned()
    }

    /// 1-based (line, col) начала узла.
    fn pos(node: TsNode<'_>) -> (u32, u32) {
        let span = ts::span(node);
        (span.start_line, span.start_col)
    }

    /// Сводит строку или f-строку к нормализованному шаблону.
    ///
    /// `"/users/{id}"` и `f"/users/{id}"` дают `/users/{id}`, а подстановки
    /// схлопываются в `{}`, чтобы литерал и f-строка на один маршрут
    /// сошлись.
    fn string_template(&self, node: TsNode<'_>) -> String {
        let mut parts: Vec<String> = Vec::new();
        for child in ts::children(node) {
            match child.kind() {
                "string_content" => parts.push(self.text(child)),
                "interpolation" => parts.push("{}".to_string()),
                _ => {}
            }
        }
        if !parts.is_empty() {
            return parts.concat();
        }
        self.text(node).trim_matches(['\'', '"']).to_string()
    }

    /// Первый позиционный аргумент; None, если первым идёт именованный.
    fn first_positional<'tree>(args: TsNode<'tree>) -> Option<TsNode<'tree>> {
        let first = ts::named_children(args).into_iter().next()?;
        if first.kind() == "keyword_argument" {
            return None;
        }
        Some(first)
    }

    fn first_string<'tree>(args: TsNode<'tree>) -> Option<TsNode<'tree>> {
        Self::first_positional(args).filter(|node| node.kind() == "string")
    }

    /// Значение именованного строкового аргумента.
    fn kwarg<'tree>(&self, args: TsNode<'tree>, wanted: &str) -> Option<TsNode<'tree>> {
        for child in ts::named_children(args) {
            if child.kind() != "keyword_argument" {
                continue;
            }
            let name = child.child_by_field_name("name")?;
            let value = child.child_by_field_name("value")?;
            if self.text(name) == wanted {
                return Some(value);
            }
        }
        None
    }

    /// Flask `methods=[...]`; по умолчанию `["GET"]`.
    fn methods_kwarg(&self, args: TsNode<'_>) -> Vec<String> {
        if let Some(value) = self.kwarg(args, "methods") {
            let verbs: Vec<String> = ts::named_children(value)
                .into_iter()
                .filter(|s| s.kind() == "string")
                .map(|s| self.string_template(s).to_uppercase())
                .collect();
            if !verbs.is_empty() {
                return verbs;
            }
        }
        vec!["GET".to_string()]
    }

    fn detail(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // -- http -------------------------------------------------------------

    /// Маршруты FastAPI / Flask / Starlette (серверная сторона).
    fn http_server(&self, decorator_calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in decorator_calls {
            let (Some(args), Some(deco), Some(func)) =
                (cap(caps, "args"), cap(caps, "deco"), cap(caps, "func"))
            else {
                continue;
            };
            let deco_text = self.text(deco);
            let method = deco_text.rsplit('.').next().unwrap_or("").to_lowercase();
            let Some(path_node) = Self::first_string(args) else {
                continue;
            };
            let path = self.string_template(path_node);
            let (line, col) = Self::pos(func);

            if HTTP_VERBS.contains(&method.as_str()) {
                refs.push(Self::http_ref(&method.to_uppercase(), &path, line, col));
            } else if method == "route" {
                for verb in self.methods_kwarg(args) {
                    refs.push(Self::http_ref(&verb, &path, line, col));
                }
            }
        }
        refs
    }

    fn http_ref(verb: &str, path: &str, line: u32, col: u32) -> BoundaryRef {
        let norm = normalize_http_path(path);
        BoundaryRef {
            mechanism: "http".to_string(),
            role: "server".to_string(),
            key: format!("{verb} {norm}"),
            line,
            col,
            confidence: 1.0,
            detail: Self::detail(&[("method", verb), ("path", &norm)]),
        }
    }

    /// Вызовы requests / httpx / сессий (клиентская сторона).
    fn http_client(&self, attr_calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in attr_calls {
            let (Some(call), Some(method_node), Some(args)) =
                (cap(caps, "call"), cap(caps, "method"), cap(caps, "args"))
            else {
                continue;
            };
            // Декоратор маршрута, а не клиентский вызов.
            if call.parent().is_some_and(|p| p.kind() == "decorator") {
                continue;
            }
            let method = self.text(method_node).to_lowercase();
            if !HTTP_VERBS.contains(&method.as_str()) {
                continue;
            }
            let Some(url_node) = Self::first_string(args) else {
                continue;
            };
            let url = self.string_template(url_node);
            // Не URL и не путь — например, dict.get("key").
            if !url.starts_with('/') && !url.contains("://") {
                continue;
            }
            let confidence = if url.starts_with('/') { 0.9 } else { 0.8 };
            let norm = normalize_http_path(&url);
            let (line, col) = Self::pos(method_node);
            let verb = method.to_uppercase();
            refs.push(BoundaryRef {
                mechanism: "http".to_string(),
                role: "client".to_string(),
                key: format!("{verb} {norm}"),
                line,
                col,
                confidence,
                detail: Self::detail(&[("method", &verb), ("path", &norm)]),
            });
        }
        refs
    }

    // -- temporal ---------------------------------------------------------

    /// Активности Temporal / DBOS: `@activity.defn` и execute-вызовы.
    fn temporal(
        &self,
        bare_decorators: &[Caps<'_, '_>],
        decorator_calls: &[Caps<'_, '_>],
        attr_calls: &[Caps<'_, '_>],
    ) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();

        for caps in bare_decorators {
            let (Some(deco), Some(func)) = (cap(caps, "deco"), cap(caps, "func")) else {
                continue;
            };
            if !Self::is_defn(&self.text(deco)) {
                continue;
            }
            refs.push(Self::temporal_server(&self.text(func), func));
        }
        for caps in decorator_calls {
            let (Some(deco), Some(func), Some(args)) =
                (cap(caps, "deco"), cap(caps, "func"), cap(caps, "args"))
            else {
                continue;
            };
            if !Self::is_defn(&self.text(deco)) {
                continue;
            }
            let name = self
                .kwarg(args, "name")
                .filter(|v| v.kind() == "string")
                .map(|v| self.string_template(v))
                .unwrap_or_else(|| self.text(func));
            refs.push(Self::temporal_server(&name, func));
        }

        for caps in attr_calls {
            let (Some(method_node), Some(args)) = (cap(caps, "method"), cap(caps, "args")) else {
                continue;
            };
            if !TEMPORAL_EXEC.contains(&self.text(method_node).as_str()) {
                continue;
            }
            let Some(name) = self.exec_target(args) else {
                continue;
            };
            let (line, col) = Self::pos(method_node);
            refs.push(BoundaryRef {
                mechanism: "temporal".to_string(),
                role: "client".to_string(),
                key: name.clone(),
                line,
                col,
                confidence: 0.9,
                detail: Self::detail(&[("activity", &name)]),
            });
        }
        refs
    }

    fn is_defn(deco_text: &str) -> bool {
        deco_text.starts_with("activity.") && deco_text.ends_with(".defn")
    }

    fn exec_target(&self, args: TsNode<'_>) -> Option<String> {
        let first = Self::first_positional(args)?;
        match first.kind() {
            "string" => Some(self.string_template(first)),
            "identifier" => Some(self.text(first)),
            "attribute" => {
                let text = self.text(first);
                Some(text.rsplit('.').next().unwrap_or(&text).to_string())
            }
            _ => None,
        }
    }

    fn temporal_server(name: &str, func: TsNode<'_>) -> BoundaryRef {
        let (line, col) = Self::pos(func);
        BoundaryRef {
            mechanism: "temporal".to_string(),
            role: "server".to_string(),
            key: name.to_string(),
            line,
            col,
            confidence: 1.0,
            detail: Self::detail(&[("activity", name)]),
        }
    }

    // -- queue ------------------------------------------------------------

    /// Продюсеры (publish/produce) и консьюмеры (subscribe) очередей.
    fn queue(&self, attr_calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in attr_calls {
            let (Some(method_node), Some(args)) = (cap(caps, "method"), cap(caps, "args")) else {
                continue;
            };
            let Some(role) = Self::queue_role(&self.text(method_node).to_lowercase()) else {
                continue;
            };
            let Some(topic_node) = Self::first_string(args) else {
                continue;
            };
            let topic = self.string_template(topic_node);
            let (line, col) = Self::pos(method_node);
            refs.push(BoundaryRef {
                mechanism: "queue".to_string(),
                role: role.to_string(),
                key: topic.clone(),
                line,
                col,
                confidence: if role == "server" { 0.75 } else { 0.7 },
                detail: Self::detail(&[("topic", &topic)]),
            });
        }
        refs
    }

    fn queue_role(method: &str) -> Option<&'static str> {
        match method {
            // Продюсер обращается к топику, консьюмер его обслуживает.
            "publish" | "produce" => Some("client"),
            "subscribe" => Some("server"),
            _ => None,
        }
    }

    // -- grpc -------------------------------------------------------------

    /// Реализации servicer (сервер) и вызовы стабов (клиент).
    fn grpc(&self, root: TsNode<'_>) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();

        for caps in ts::run_query(cached_query!(Q_SERVICER_CLASS), root, self.source) {
            let (Some(bases), Some(body)) = (cap(&caps, "bases"), cap(&caps, "body")) else {
                continue;
            };
            let Some(service) = self.service_of(bases) else {
                continue;
            };
            for method_def in Self::class_methods(body) {
                let Some(name_node) = method_def.child_by_field_name("name") else {
                    continue;
                };
                let method = self.text(name_node);
                if method.starts_with("__") {
                    continue;
                }
                refs.push(Self::grpc_ref("server", &service, &method, name_node));
            }
        }

        let mut stubs: BTreeMap<String, String> = BTreeMap::new();
        for caps in ts::run_query(cached_query!(Q_STUB_ASSIGN), root, self.source) {
            let (Some(var), Some(callee)) = (cap(&caps, "var"), cap(&caps, "callee")) else {
                continue;
            };
            if let Some(service) = Self::strip_suffix(&self.text(callee), "Stub") {
                stubs.insert(self.text(var), service);
            }
        }
        for caps in ts::run_query(cached_query!(Q_OBJ_CALL), root, self.source) {
            let (Some(obj), Some(method_node)) = (cap(&caps, "obj"), cap(&caps, "method")) else {
                continue;
            };
            let Some(service) = stubs.get(&self.text(obj)) else {
                continue;
            };
            refs.push(Self::grpc_ref(
                "client",
                service,
                &self.text(method_node),
                method_node,
            ));
        }
        refs
    }

    /// Ведущее имя сервиса, если текст оканчивается на суффикс.
    fn strip_suffix(text: &str, suffix: &str) -> Option<String> {
        let last = text.rsplit('.').next().unwrap_or(text);
        let stem = last.strip_suffix(suffix)?;
        if stem.is_empty() {
            return None;
        }
        Some(stem.to_string())
    }

    fn service_of(&self, bases: TsNode<'_>) -> Option<String> {
        ts::named_children(bases)
            .into_iter()
            .find_map(|base| Self::strip_suffix(&self.text(base), "Servicer"))
    }

    /// function_definition непосредственно в теле класса.
    fn class_methods<'tree>(body: TsNode<'tree>) -> Vec<TsNode<'tree>> {
        let mut methods = Vec::new();
        for child in ts::children(body) {
            match child.kind() {
                "function_definition" => methods.push(child),
                "decorated_definition" => {
                    if let Some(inner) = child.child_by_field_name("definition")
                        && inner.kind() == "function_definition"
                    {
                        methods.push(inner);
                    }
                }
                _ => {}
            }
        }
        methods
    }

    fn grpc_ref(role: &str, service: &str, method: &str, node: TsNode<'_>) -> BoundaryRef {
        let (line, col) = Self::pos(node);
        BoundaryRef {
            mechanism: "grpc".to_string(),
            role: role.to_string(),
            key: format!("{service}/{method}"),
            line,
            col,
            confidence: 0.85,
            detail: Self::detail(&[("service", service), ("method", method)]),
        }
    }
}

/// Все границы в одном разобранном файле.
///
/// Порядок экстракторов тот же, что в graphlens
/// (`PY_DEFAULT_BOUNDARY_EXTRACTORS`), потому что от него зависит порядок
/// рёбер в графе.
pub fn extract_boundaries(root: TsNode<'_>, source: &[u8]) -> Vec<BoundaryRef> {
    let extractor = Extractor { source };

    // Запросы, нужные нескольким экстракторам, прогоняются по одному разу:
    // Q_ATTR_CALL раньше обходил дерево трижды (http-клиент, temporal,
    // очереди), Q_DECORATOR_CALL — дважды.
    let decorator_calls = ts::run_query(cached_query!(Q_DECORATOR_CALL), root, source);
    let bare_decorators = ts::run_query(cached_query!(Q_DECORATOR_BARE), root, source);
    let attr_calls = ts::run_query(cached_query!(Q_ATTR_CALL), root, source);

    let mut refs = extractor.http_server(&decorator_calls);
    refs.extend(extractor.http_client(&attr_calls));
    refs.extend(extractor.temporal(&bare_decorators, &decorator_calls, &attr_calls));
    refs.extend(extractor.queue(&attr_calls));
    refs.extend(extractor.grpc(root));
    refs
}

/// Границы в одном исходнике — точка входа для проверок и внешних вызовов.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn extract_python_boundaries(source: &Bound<'_, PyBytes>) -> PyResult<Vec<BoundaryRef>> {
    let source = source.as_bytes();
    let tree = super::parse_tree(source)?;
    Ok(extract_boundaries(tree.root_node(), source))
}
