//! Cross-language boundary extraction from Python sources.
//!
//! Each extractor recognizes one boundary *mechanism* and returns
//! language-independent `BoundaryRef`s (server provides, client consumes).
//! The adapter turns them into BOUNDARY nodes and EXPOSES / CONSUMES edges,
//! so a Python server and, say, a TypeScript client meet in one graph node.
//!
//! Patterns are written as declarative tree-sitter queries rather than as
//! branches in a hand-written visitor.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use indexmap::IndexMap;

use crate::py::prelude::*;
use crate::py::types::PyBytes;
use crate::py::derive::gen_stub_pyfunction;
use tree_sitter::{Node as TsNode, Query};

use crate::boundaries::{BoundaryRef, join_http_path as join_paths, normalize_http_path};
use crate::ts;

const HTTP_VERBS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
const TEMPORAL_EXEC: [&str; 4] = [
    "execute_activity",
    "execute_local_activity",
    "start_activity",
    "execute_activity_method",
];

/// A decorator call: `@app.get("/x")` / `@activity.defn(name=...)`.
const Q_DECORATOR_CALL: &str = r#"
(decorated_definition
  (decorator (call
    function: (attribute) @deco
    arguments: (argument_list) @args))
  definition: (function_definition name: (identifier) @func))
"#;
/// A bare attribute decorator: `@activity.defn`.
const Q_DECORATOR_BARE: &str = r#"
(decorated_definition
  (decorator (attribute) @deco)
  definition: (function_definition name: (identifier) @func))
"#;
/// An attribute call: `requests.get("/x")` / `workflow.execute_activity(a)`.
const Q_ATTR_CALL: &str = r#"
(call
  function: (attribute attribute: (identifier) @method)
  arguments: (argument_list) @args) @call
"#;
// gRPC: protoc generates `<Service>Servicer` bases and `<Service>Stub` stubs.
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

/// Compiling a query costs more than running it, so it happens once per
/// process.
macro_rules! cached_query {
    ($source:expr) => {{
        static QUERY: OnceLock<Query> = OnceLock::new();
        QUERY.get_or_init(|| {
            Query::new(&tree_sitter_python::LANGUAGE.into(), $source)
                .expect("the query is valid for the python grammar")
        })
    }};
}

/// One match's capture map: `@name` → nodes.
type Caps<'q, 'tree> = HashMap<&'q str, Vec<TsNode<'tree>>>;

/// The first node under a capture.
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

    /// The node's 1-based start (line, col).
    fn pos(node: TsNode<'_>) -> (u32, u32) {
        let span = ts::span(node);
        (span.start_line, span.start_col)
    }

    /// Reduces a string or f-string to a normalized template.
    ///
    /// `"/users/{id}"` and `f"/users/{id}"` both yield `/users/{id}`, and
    /// interpolations collapse into `{}` so a literal and an f-string for the
    /// same route meet.
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

    /// The first positional argument; None if a keyword argument comes first.
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

    /// The value of a named string argument.
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

    /// Flask's `methods=[...]`; defaults to `["GET"]`.
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

    /// The URL prefix of every router in the file, by the name it is bound to.
    ///
    /// `router = APIRouter(prefix="/users")` and `Blueprint(..., url_prefix=)`
    /// fix a prefix at construction, and `app.include_router(router,
    /// prefix="/api")` / `app.register_blueprint(bp, url_prefix=)` put another
    /// one in front. Only what the same file says is known: a router built in
    /// one module and mounted in another is not followed.
    fn router_prefixes(&self, root: TsNode<'_>) -> HashMap<String, String> {
        const PREFIX_KWARGS: [&str; 2] = ["prefix", "url_prefix"];
        let mut own: HashMap<String, String> = HashMap::new();
        let mut mounted: HashMap<String, String> = HashMap::new();

        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            stack.extend(ts::children(node));
            match node.kind() {
                "assignment" => {
                    let (Some(left), Some(right)) =
                        (node.child_by_field_name("left"), node.child_by_field_name("right"))
                    else {
                        continue;
                    };
                    if left.kind() != "identifier" || right.kind() != "call" {
                        continue;
                    }
                    let Some(args) = right.child_by_field_name("arguments") else { continue };
                    for kwarg in PREFIX_KWARGS {
                        if let Some(value) = self.kwarg(args, kwarg).filter(|v| v.kind() == "string") {
                            own.insert(self.text(left), self.string_template(value));
                        }
                    }
                }
                "call" => {
                    let Some(function) = node.child_by_field_name("function") else { continue };
                    if function.kind() != "attribute" {
                        continue;
                    }
                    let method = function
                        .child_by_field_name("attribute")
                        .map(|n| self.text(n))
                        .unwrap_or_default();
                    if method != "include_router" && method != "register_blueprint" {
                        continue;
                    }
                    let Some(args) = node.child_by_field_name("arguments") else { continue };
                    let Some(target) = Self::first_positional(args).filter(|n| n.kind() == "identifier")
                    else {
                        continue;
                    };
                    for kwarg in PREFIX_KWARGS {
                        if let Some(value) = self.kwarg(args, kwarg).filter(|v| v.kind() == "string") {
                            mounted.insert(self.text(target), self.string_template(value));
                        }
                    }
                }
                _ => {}
            }
        }

        let mut names: Vec<&String> = own.keys().chain(mounted.keys()).collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .map(|name| {
                let outer = mounted.get(name).map_or("", String::as_str);
                let inner = own.get(name).map_or("", String::as_str);
                (name.clone(), join_paths(outer, inner))
            })
            .collect()
    }

    /// FastAPI / Flask / Starlette routes (the server side).
    fn http_server(
        &self,
        decorator_calls: &[Caps<'_, '_>],
        prefixes: &HashMap<String, String>,
    ) -> Vec<BoundaryRef> {
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
            // `router.get("/x")`: the object the route is registered on may
            // carry a prefix. The prefixes this file knows are applied here,
            // so a single source is right on its own; the router and the path
            // as written are left in the detail for `routers::apply`, which
            // follows the router across modules when a whole project is walked.
            let receiver = deco_text.rsplit_once('.').map_or("", |(object, _)| object);
            let route = self.string_template(path_node);
            let path = join_paths(prefixes.get(receiver).map_or("", String::as_str), &route);
            let (line, col) = Self::pos(func);

            let mut found = Vec::new();
            if HTTP_VERBS.contains(&method.as_str()) {
                found.push(Self::http_ref(&method.to_uppercase(), &path, line, col));
            } else if method == "route" {
                for verb in self.methods_kwarg(args) {
                    found.push(Self::http_ref(&verb, &path, line, col));
                }
            }
            for mut reference in found {
                if !receiver.is_empty() {
                    reference.detail.insert("router".to_string(), receiver.to_string());
                    reference.detail.insert("route".to_string(), route.clone());
                }
                refs.push(reference);
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

    /// requests / httpx / session calls (the client side).
    fn http_client(&self, attr_calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in attr_calls {
            let (Some(call), Some(method_node), Some(args)) =
                (cap(caps, "call"), cap(caps, "method"), cap(caps, "args"))
            else {
                continue;
            };
            // A route decorator, not a client call.
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
            // Neither a URL nor a path — dict.get("key"), for instance.
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

    /// Temporal / DBOS activities: `@activity.defn` and execute calls.
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

    /// Queue producers (publish/produce) and consumers (subscribe).
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
            // A producer addresses the topic, a consumer serves it.
            // The names span the clients people actually use rather than the
            // two that read most like English: sarama says `SendMessage`, AMQP
            // `basic_publish`, Redis Streams `xadd`, task queues `enqueue` and
            // `delay`.
            //
            // A bare `send` is deliberately absent: it is also `res.send(...)`
            // in every Express handler and `conn.send(...)` on every socket,
            // and the verb alone cannot tell them apart. Those clients need a
            // custom extractor, which the adapters accept.
            "publish" | "produce" | "sendmessage" | "basic_publish" | "xadd" | "enqueue"
            | "delay" | "emit" => Some("client"),
            "subscribe" | "consume" | "basic_consume" | "xreadgroup" => Some("server"),
            _ => None,
        }
    }

    // -- grpc -------------------------------------------------------------

    /// Servicer implementations (server) and stub calls (client).
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

    /// The leading service name, if the text ends with the suffix.
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

    /// A function_definition directly in a class body.
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

/// Every boundary in one parsed file.
///
/// The extractor order matches graphlens's
/// (`PY_DEFAULT_BOUNDARY_EXTRACTORS`), because edge order in the graph
/// depends on it.
pub fn extract_boundaries(root: TsNode<'_>, source: &[u8]) -> Vec<BoundaryRef> {
    let extractor = Extractor { source };

    // Queries several extractors need are run once each: Q_ATTR_CALL used to
    // walk the tree three times (http client, temporal, queues) and
    // Q_DECORATOR_CALL twice.
    let decorator_calls = ts::run_query(cached_query!(Q_DECORATOR_CALL), root, source);
    let bare_decorators = ts::run_query(cached_query!(Q_DECORATOR_BARE), root, source);
    let attr_calls = ts::run_query(cached_query!(Q_ATTR_CALL), root, source);

    let prefixes = extractor.router_prefixes(root);
    let mut refs = extractor.http_server(&decorator_calls, &prefixes);
    refs.extend(extractor.http_client(&attr_calls));
    refs.extend(extractor.temporal(&bare_decorators, &decorator_calls, &attr_calls));
    refs.extend(extractor.queue(&attr_calls));
    refs.extend(extractor.grpc(root));
    refs
}

/// Boundaries in a single source — the entry point for checks and external
/// callers.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn extract_python_boundaries(source: &Bound<'_, PyBytes>) -> PyResult<Vec<BoundaryRef>> {
    let source = source.as_bytes();
    let tree = super::parse_tree(source)?;
    let mut refs = extract_boundaries(tree.root_node(), source);
    // `router` and `route` are for the adapter, which follows a router across
    // modules; a single source has nothing to follow them with.
    for reference in &mut refs {
        reference.detail.shift_remove("router");
        reference.detail.shift_remove("route");
    }
    Ok(refs)
}
