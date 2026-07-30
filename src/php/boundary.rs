//! Cross-language boundary extraction from PHP.
//!
//! Patterns are declarative tree-sitter queries compiled once, not branches in
//! a hand-written visitor, and the decision "route or client call?" is made on
//! the SHAPE of the arguments — a bound handler versus a URL. A whitelist of
//! receiver names (`$app`, `$client`, `Route`) was removed from the Go and
//! TypeScript extractors because it both missed real boundaries and invented
//! false ones; PHP never had one.
//!
//! Interpolation is collapsed the way `string_template` does it in the Python
//! extractor: `"/engines/{$id}"` yields `/engines/{}`, which is the same key a
//! literal `/engines/{id}` normalizes to. Without that a PHP client and a
//! Python server describing one route would land on two BOUNDARY nodes.

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

/// Laravel's `Route::any` and Slim's `$app->any` answer every method, and so
/// does a Symfony `#[Route]` that names none.
const ANY_METHOD: &str = "ANY";

/// `$client->get(…)`, `$app->get(…)`, `$queue?->publish(…)`.
///
/// The receiver is `(_)` and uncaptured: a router reached through a property
/// (`$this->app->get(…)`) is as common as one in a local variable, and the
/// name it carries decides nothing here.
const Q_MEMBER_CALL: &str = r#"
(member_call_expression
  object: (_)
  name: (name) @method
  arguments: (arguments) @args)
(nullsafe_member_call_expression
  object: (_)
  name: (name) @method
  arguments: (arguments) @args)
"#;
/// `Route::get(…)`, `Queue::push(…)` — a static call on a class.
const Q_SCOPED_CALL: &str = r#"
(scoped_call_expression
  scope: (_)
  name: (name) @method
  arguments: (arguments) @args)
"#;
/// A global function: `file_get_contents(…)`, `curl_setopt(…)`, `dispatch(…)`.
const Q_FUNCTION_CALL: &str = r#"
(function_call_expression
  function: [(name) (qualified_name)] @callee
  arguments: (arguments) @args)
"#;
/// Symfony's `#[Route('/x', methods: ['GET'])]`.
///
/// The argument list is under `parameters:`, the one node in this grammar that
/// spells it that way instead of `arguments:`.
const Q_ATTRIBUTE: &str = r#"
(attribute
  [(name) (qualified_name) (relative_name)] @attr
  parameters: (arguments) @args)
"#;

/// Compiling a query costs more than running it, so it happens once per
/// process.
macro_rules! cached_query {
    ($source:expr) => {{
        static QUERY: OnceLock<Query> = OnceLock::new();
        QUERY.get_or_init(|| {
            Query::new(&tree_sitter_php::LANGUAGE_PHP.into(), $source)
                .expect("the query is valid for the php grammar")
        })
    }};
}

/// One match's capture map: `@name` → nodes.
type Caps<'q, 'tree> = HashMap<&'q str, Vec<TsNode<'tree>>>;

fn cap<'tree>(caps: &Caps<'_, 'tree>, name: &str) -> Option<TsNode<'tree>> {
    caps.get(name).and_then(|nodes| nodes.first()).copied()
}

/// What the second argument of `verb(path, …)` says about the call.
///
/// This is the whole route-versus-client decision, and it reads the argument
/// rather than the receiver.
enum Shape {
    /// Something is being bound: a closure, `[Controller::class, 'show']`,
    /// `'Controller@show'`.
    Handler,
    /// Options for a request: an associative array, a plain string, a number.
    Options,
    /// A variable or an expression — the shape decides nothing.
    Ambiguous,
}

/// One argument of a call: the expression, plus its `name:` label when the
/// call site used a named argument.
struct Arg<'tree> {
    label: Option<String>,
    value: TsNode<'tree>,
}

/// A queue verb's side of the contract.
///
/// The table is the one the Python and Go extractors use, so a PHP producer
/// and a Go consumer of one topic meet on the same node. A bare `send` is
/// deliberately absent: it is also every socket's `send`, and the verb alone
/// cannot tell them apart.
fn queue_role(method: &str) -> Option<&'static str> {
    match method {
        "publish" | "produce" | "sendmessage" | "basic_publish" | "xadd" | "enqueue"
        | "delay" | "emit" => Some("client"),
        "subscribe" | "consume" | "basic_consume" | "xreadgroup" => Some("server"),
        _ => None,
    }
}

/// Laravel dispatches a job by handing over the object itself, so the queue's
/// name is the job class rather than a topic string.
fn dispatches_job(method: &str) -> bool {
    matches!(method, "push" | "pushon" | "later" | "dispatch" | "dispatchnow")
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

    /// A string literal reduced to a normalized template, or None when the
    /// node is not a string at all.
    ///
    /// `'/x'`, `"/x/{$id}"`, `<<<SQL` and `<<<'SQL'` are the four spellings;
    /// every interpolation collapses to `{}`, exactly as the Python extractor
    /// does for an f-string, so both sides of a route meet on one key.
    fn string_template(&self, node: TsNode<'_>) -> Option<String> {
        match node.kind() {
            "string" | "encapsed_string" => {}
            // The body carries the content; the tags carry the delimiters.
            "heredoc" | "nowdoc" => {
                return match node.child_by_field_name("value") {
                    Some(body) => self.string_template(body),
                    // An empty heredoc has no body node.
                    None => Some(String::new()),
                };
            }
            "heredoc_body" | "nowdoc_body" => {}
            _ => return None,
        }

        let mut parts: Vec<String> = Vec::new();
        for child in ts::children(node) {
            match child.kind() {
                "string_content" | "escape_sequence" => parts.push(self.text(child)),
                // `{`, `}` and the quotes are anonymous; every named child
                // left is an interpolated expression.
                _ if child.is_named() => parts.push("{}".to_string()),
                _ => {}
            }
        }
        Some(parts.concat())
    }

    /// The arguments of a call, in source order.
    ///
    /// The value is the last named child: a named argument is
    /// `[label, ':', value]` and `&$x` is `[reference_modifier, value]`, so
    /// reading from the end serves both.
    fn arguments<'tree>(&self, arguments: TsNode<'tree>) -> Vec<Arg<'tree>> {
        let mut out = Vec::new();
        for argument in ts::children(arguments) {
            if argument.kind() != "argument" {
                continue;
            }
            let label = argument.child_by_field_name("name").map(|n| self.text(n));
            if let Some(value) = ts::named_children(argument).into_iter().next_back()
                && Some(value) != argument.child_by_field_name("name")
            {
                out.push(Arg { label, value });
            }
        }
        out
    }

    /// The n-th argument that carries no label.
    fn positional<'tree>(args: &[Arg<'tree>], index: usize) -> Option<TsNode<'tree>> {
        args.iter()
            .filter(|arg| arg.label.is_none())
            .nth(index)
            .map(|arg| arg.value)
    }

    /// The value of a named argument, `methods: ['GET']`.
    fn named<'tree>(args: &[Arg<'tree>], wanted: &str) -> Option<TsNode<'tree>> {
        args.iter()
            .find(|arg| arg.label.as_deref() == Some(wanted))
            .map(|arg| arg.value)
    }

    /// The strings of an array literal, or the single string itself.
    ///
    /// Symfony accepts both `methods: ['GET']` and `methods: 'GET'`.
    fn string_list(&self, node: TsNode<'_>) -> Vec<String> {
        if let Some(one) = self.string_template(node) {
            return vec![one];
        }
        ts::named_children(node)
            .into_iter()
            .filter_map(|element| {
                ts::named_children(element)
                    .into_iter()
                    .next()
                    .and_then(|value| self.string_template(value))
            })
            .collect()
    }

    /// Whether an array literal has `key => value` entries.
    ///
    /// This is what separates a request's options (`['json' => $body]`) from a
    /// bound handler (`[Controller::class, 'show']`) without looking at either
    /// side's names.
    fn is_keyed_array(node: TsNode<'_>) -> bool {
        ts::named_children(node)
            .into_iter()
            .any(|element| ts::named_children(element).len() > 1)
    }

    fn shape(&self, node: TsNode<'_>) -> Shape {
        match node.kind() {
            "anonymous_function" | "arrow_function" => Shape::Handler,
            // `Controller::class`, used alone or inside a callable array.
            "class_constant_access_expression" => Shape::Handler,
            "array_creation_expression" => {
                if Self::is_keyed_array(node) { Shape::Options } else { Shape::Handler }
            }
            // `'App\Http\Controller@show'` and `'Controller::show'` name an
            // action; any other string is data.
            "string" | "encapsed_string" => {
                let text = self.text(node);
                if text.contains('@') || text.contains("::") {
                    Shape::Handler
                } else {
                    Shape::Options
                }
            }
            "name" | "qualified_name" | "relative_name" => Shape::Handler,
            _ => Shape::Ambiguous,
        }
    }

    /// Which side of the contract a `verb(path, …)` call sits on.
    ///
    /// A bound handler makes it a route; an options array or nothing at all
    /// makes it a request. `Ambiguous` — `verb('/x', $second)` — is decided by
    /// the call form, which is structure rather than a name: `::` reaches a
    /// class, and a static verb call with a second argument is a registration
    /// (`Route::get('/x', $handler)`), while `->` reaches an object, and an
    /// HTTP client is always an object (`$client->get('/x', $options)`).
    ///
    /// A client's confidence follows the Python extractor's: a relative path is
    /// worth more than an absolute URL, whose host may name a third party
    /// rather than a service in this graph.
    fn route_role(&self, args: &[Arg<'_>], is_static: bool, path: &str) -> (&'static str, f64) {
        match Self::positional(args, 1).map(|arg| self.shape(arg)) {
            Some(Shape::Handler) => ("server", 1.0),
            Some(Shape::Ambiguous) if is_static => ("server", 0.75),
            _ => ("client", if path.starts_with('/') { 0.9 } else { 0.8 }),
        }
    }

    fn detail(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn http_ref(
        role: &str,
        verb: &str,
        path: &str,
        confidence: f64,
        node: TsNode<'_>,
    ) -> BoundaryRef {
        let norm = normalize_http_path(path);
        let (line, col) = Self::pos(node);
        BoundaryRef {
            mechanism: "http".to_string(),
            role: role.to_string(),
            key: format!("{verb} {norm}"),
            line,
            col,
            confidence,
            detail: Self::detail(&[("method", verb), ("path", &norm)]),
        }
    }

    // -- http ---------------------------------------------------------------

    /// Routes and requests written as a method call.
    ///
    /// One pass over one query serves both sides, because the same three
    /// tokens — a verb, a path and what follows it — decide which side a call
    /// is on.
    fn http_calls(&self, calls: &[Caps<'_, '_>], is_static: bool) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(method_node), Some(args_node)) = (cap(caps, "method"), cap(caps, "args"))
            else {
                continue;
            };
            let method = self.text(method_node).to_lowercase();
            let args = self.arguments(args_node);

            if HTTP_VERBS.contains(&method.as_str()) {
                let Some(path) = Self::positional(&args, 0).and_then(|n| self.string_template(n))
                else {
                    continue;
                };
                // Neither a route nor a URL — `$cache->get('key')`.
                if !path.starts_with('/') && !path.contains("://") {
                    continue;
                }
                let (role, confidence) = self.route_role(&args, is_static, &path);
                refs.push(Self::http_ref(
                    role,
                    &method.to_uppercase(),
                    &path,
                    confidence,
                    method_node,
                ));
                continue;
            }

            // `Route::any('/x', $handler)` — a route on every method. There is
            // no client counterpart, so an argument list that reads as a
            // request is not a boundary at all.
            if method == "any" {
                let Some(path) = Self::positional(&args, 0).and_then(|n| self.string_template(n))
                else {
                    continue;
                };
                let (role, confidence) = self.route_role(&args, is_static, &path);
                if role == "server" {
                    refs.push(Self::http_ref(role, ANY_METHOD, &path, confidence, method_node));
                }
                continue;
            }

            // `Route::match(['GET', 'POST'], '/x', $handler)` — the verbs come
            // first, so the path is the second argument.
            if method == "match" {
                let (Some(verbs), Some(path)) = (
                    Self::positional(&args, 0).map(|n| self.string_list(n)),
                    Self::positional(&args, 1).and_then(|n| self.string_template(n)),
                ) else {
                    continue;
                };
                for verb in verbs {
                    refs.push(Self::http_ref(
                        "server",
                        &verb.to_uppercase(),
                        &path,
                        1.0,
                        method_node,
                    ));
                }
                continue;
            }

            // Guzzle's generic form: `$client->request('GET', $url)`.
            if method == "request"
                && let Some(verb) = Self::positional(&args, 0).and_then(|n| self.string_template(n))
                && HTTP_VERBS.contains(&verb.to_lowercase().as_str())
                && let Some(url) = Self::positional(&args, 1).and_then(|n| self.string_template(n))
            {
                refs.push(Self::http_ref(
                    "client",
                    &verb.to_uppercase(),
                    &url,
                    0.9,
                    method_node,
                ));
            }
        }
        refs
    }

    /// Symfony's route attributes: `#[Route('/x', methods: ['GET'])]` and the
    /// per-verb shorthands `#[Get('/x')]`.
    ///
    /// The attribute's own class name is the mechanism marker here — an
    /// attribute binds no handler to read, because the declaration it sits on
    /// *is* the handler.
    fn http_attributes(&self, attributes: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in attributes {
            let (Some(attr), Some(args_node)) = (cap(caps, "attr"), cap(caps, "args")) else {
                continue;
            };
            // `#[Symfony\Component\Routing\Annotation\Route(…)]` is the same
            // attribute as an imported `#[Route(…)]`.
            let text = self.text(attr);
            let name = text.rsplit('\\').next().unwrap_or(&text).to_lowercase();
            let args = self.arguments(args_node);
            // `#[Route(path: '/x')]` names the path; the shorthands and the
            // usual form put it first.
            let Some(path) = Self::named(&args, "path")
                .or_else(|| Self::positional(&args, 0))
                .and_then(|n| self.string_template(n))
            else {
                continue;
            };

            if name == "route" {
                let verbs = Self::named(&args, "methods")
                    .map(|n| self.string_list(n))
                    .filter(|verbs| !verbs.is_empty())
                    // A Symfony route without `methods` answers all of them.
                    .unwrap_or_else(|| vec![ANY_METHOD.to_string()]);
                for verb in verbs {
                    refs.push(Self::http_ref(
                        "server",
                        &verb.to_uppercase(),
                        &path,
                        1.0,
                        attr,
                    ));
                }
            } else if HTTP_VERBS.contains(&name.as_str()) {
                refs.push(Self::http_ref("server", &name.to_uppercase(), &path, 1.0, attr));
            }
        }
        refs
    }

    /// The two HTTP clients that are plain function calls.
    fn http_functions(&self, calls: &[Caps<'_, '_>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(callee), Some(args_node)) = (cap(caps, "callee"), cap(caps, "args")) else {
                continue;
            };
            let text = self.text(callee);
            let name = text.rsplit('\\').next().unwrap_or(&text).to_lowercase();
            let args = self.arguments(args_node);

            // `file_get_contents` reads a file just as often as a URL, so the
            // scheme has to be there: a bare path is not a boundary.
            if name == "file_get_contents"
                && let Some(url) = Self::positional(&args, 0).and_then(|n| self.string_template(n))
                && url.contains("://")
            {
                refs.push(Self::http_ref("client", "GET", &url, 0.8, callee));
                continue;
            }

            // `curl_setopt($h, CURLOPT_URL, $url)`. The verb is not in this
            // call — a POST is a second `curl_setopt` on the same handle — so
            // the request is recorded as the GET that curl defaults to.
            if name == "curl_setopt"
                && Self::positional(&args, 1)
                    .map(|n| self.text(n).to_uppercase())
                    .is_some_and(|option| option.ends_with("CURLOPT_URL"))
                && let Some(url) = Self::positional(&args, 2).and_then(|n| self.string_template(n))
            {
                refs.push(Self::http_ref("client", "GET", &url, 0.7, callee));
            }
        }
        refs
    }

    // -- queue --------------------------------------------------------------

    /// Producers and consumers of a topic, plus Laravel's job dispatches.
    fn queue(
        &self,
        member_calls: &[Caps<'_, '_>],
        scoped_calls: &[Caps<'_, '_>],
        function_calls: &[Caps<'_, '_>],
    ) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in member_calls.iter().chain(scoped_calls) {
            let (Some(method_node), Some(args_node)) = (cap(caps, "method"), cap(caps, "args"))
            else {
                continue;
            };
            let method = self.text(method_node).to_lowercase();
            let args = self.arguments(args_node);

            if let Some(role) = queue_role(&method)
                && let Some(topic) = Self::positional(&args, 0).and_then(|n| self.string_template(n))
            {
                refs.push(Self::topic_ref(role, &topic, method_node));
                continue;
            }
            if dispatches_job(&method)
                && let Some(job) = self.dispatched_job(&args)
            {
                refs.push(Self::job_ref(&job, method_node));
            }
        }

        for caps in function_calls {
            let (Some(callee), Some(args_node)) = (cap(caps, "callee"), cap(caps, "args")) else {
                continue;
            };
            let text = self.text(callee);
            let name = text.rsplit('\\').next().unwrap_or(&text).to_lowercase();
            if dispatches_job(&name)
                && let Some(job) = self.dispatched_job(&self.arguments(args_node))
            {
                refs.push(Self::job_ref(&job, callee));
            }
        }
        refs
    }

    /// The class of the job an argument list hands over.
    ///
    /// The position is not fixed — `Queue::later($delay, new Job)` puts it
    /// second — so the first `new` in the list is taken.
    fn dispatched_job(&self, args: &[Arg<'_>]) -> Option<String> {
        for arg in args {
            if arg.value.kind() != "object_creation_expression" {
                continue;
            }
            let class = ts::child_of_types(
                arg.value,
                &["name", "qualified_name", "relative_name"],
            )?;
            let text = self.text(class);
            return Some(text.rsplit('\\').next().unwrap_or(&text).to_string());
        }
        None
    }

    fn topic_ref(role: &str, topic: &str, node: TsNode<'_>) -> BoundaryRef {
        let (line, col) = Self::pos(node);
        BoundaryRef {
            mechanism: "queue".to_string(),
            role: role.to_string(),
            key: topic.to_string(),
            line,
            col,
            confidence: if role == "server" { 0.75 } else { 0.7 },
            detail: Self::detail(&[("topic", topic)]),
        }
    }

    /// A dispatched job, keyed by its class's short name — that is the topic a
    /// worker in another language would name.
    fn job_ref(job: &str, node: TsNode<'_>) -> BoundaryRef {
        let (line, col) = Self::pos(node);
        BoundaryRef {
            mechanism: "queue".to_string(),
            role: "client".to_string(),
            key: job.to_string(),
            line,
            col,
            confidence: 0.7,
            detail: Self::detail(&[("job", job)]),
        }
    }
}

/// Every boundary in one file.
///
/// The extractor order is part of the output: EXPOSES / CONSUMES edges land in
/// the graph in the order they are found here.
pub fn extract_boundaries(root: TsNode<'_>, source: &[u8]) -> Vec<BoundaryRef> {
    let extractor = Extractor { source };

    // Each query is run once and shared: the queue extractor reads the same
    // three call queries the HTTP ones do.
    let member_calls = ts::run_query(cached_query!(Q_MEMBER_CALL), root, source);
    let scoped_calls = ts::run_query(cached_query!(Q_SCOPED_CALL), root, source);
    let function_calls = ts::run_query(cached_query!(Q_FUNCTION_CALL), root, source);
    let attributes = ts::run_query(cached_query!(Q_ATTRIBUTE), root, source);

    let mut refs = extractor.http_attributes(&attributes);
    refs.extend(extractor.http_calls(&scoped_calls, true));
    refs.extend(extractor.http_calls(&member_calls, false));
    refs.extend(extractor.http_functions(&function_calls));
    refs.extend(extractor.queue(&member_calls, &scoped_calls, &function_calls));
    refs
}

/// Boundaries in a single source — the entry point for checks.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn extract_php_boundaries(source: &Bound<'_, PyBytes>) -> PyResult<Vec<BoundaryRef>> {
    let source = source.as_bytes();
    let tree = super::parse_tree(source)?;
    Ok(extract_boundaries(tree.root_node(), source))
}
