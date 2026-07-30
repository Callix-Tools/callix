//! Cross-language boundary extraction from C and C++.
//!
//! The family's HTTP boundaries look nothing like the other languages'. There
//! is no framework everybody uses and no decorator to read: a client is
//! libcurl, and libcurl takes its URL as the THIRD argument of a variadic
//! setter whose second argument is an option constant. So detection here is
//! argument shape in the strictest sense — `(handle, CURLOPT_URL, url)` — and
//! never the name of a receiver.
//!
//! The harder half is that the URL is rarely a literal at the call. C builds it
//! with `snprintf` into a buffer and passes the buffer; C++ concatenates a
//! literal with `std::to_string(...)`. Both are reconstructed into a pattern
//! whose variable parts are `{}`, which is exactly what `normalize_http_path`
//! produces from `/users/{id}` — so `snprintf(url, n, "/engines/%ld", id)`
//! meets the `GET /engines/{}` a Python route or an OpenAPI path declares. When
//! nothing usable can be recovered, nothing is emitted: a key of `/{}` or `/`
//! would merge unrelated services into one node, which is worse than a missing
//! edge.
//!
//! Servers: civetweb's `mg_set_request_handler`, libmicrohttpd's
//! `MHD_start_daemon`, cpp-httplib's `svr.Get(path, handler)`, Crow's
//! `CROW_ROUTE(app, path)` macro, Drogon's `ADD_METHOD_TO`, and a gRPC service
//! implementation (a class inheriting `X::Service`). The API names of the C
//! libraries are matched, and that is not the whitelist the Go and TypeScript
//! extractors dropped: `mg_set_request_handler` is spelled by civetweb and
//! cannot be renamed, while the `r` in `r.GET(...)` was a variable somebody
//! chose. Wherever a name IS chosen — the receiver of `svr.Get`, the stub of
//! `stub->SayHello` — it is the argument shape that decides.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use tree_sitter::{Node as TsNode, Query};

use crate::boundaries::{BoundaryRef, normalize_http_path};
use crate::ts;

use super::Dialect;

/// What an unrecoverable part of a URL becomes — the spelling
/// `normalize_http_path` reduces every path parameter to.
const PLACEHOLDER: &str = "{}";

/// HTTP verbs, lowercased, as a method name is compared.
const HTTP_VERBS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

/// How far a URL is followed through variables and concatenations.
const MAX_URL_DEPTH: usize = 6;

/// How far the search for Crow's `.methods(...)` climbs out of `CROW_ROUTE`.
const MAX_CHAIN_CLIMB: usize = 6;

/// The libcurl option whose value is the URL. Its value is the THIRD argument
/// of the setter, which is why position rather than a name is what is matched.
const CURL_URL_OPTION: &str = "CURLOPT_URL";
const CURL_URL_ARGUMENT: usize = 2;

/// The libcurl option that spells the verb out.
const CURL_CUSTOM_REQUEST: &str = "CURLOPT_CUSTOMREQUEST";

/// libcurl options that imply a verb. `CURLOPT_HTTPGET` is absent because GET
/// is already the default and saying so changes nothing.
const CURL_VERB_OPTIONS: [(&str, &str); 6] = [
    ("CURLOPT_POST", "POST"),
    ("CURLOPT_POSTFIELDS", "POST"),
    ("CURLOPT_HTTPPOST", "POST"),
    ("CURLOPT_UPLOAD", "PUT"),
    ("CURLOPT_PUT", "PUT"),
    ("CURLOPT_NOBODY", "HEAD"),
];

/// Calls that append to their first argument rather than overwrite it.
const APPEND_FUNCTIONS: [&str; 4] = ["strcat", "strncat", "strlcat", "stpcat"];

/// Accessors that hand back the buffer of a string object, so the URL is
/// whatever the object held.
const STRING_ACCESSORS: [&str; 4] = ["c_str", "data", "str", "string"];

/// civetweb registers `(context, uri, handler, cbdata)`.
const CIVETWEB_HANDLER: &str = "mg_set_request_handler";

/// libmicrohttpd starts a daemon with one handler for every URL.
const MICROHTTPD_DAEMON: &str = "MHD_start_daemon";

/// Crow's route macro, which the grammar reads as an ordinary call.
const CROW_ROUTE: &str = "CROW_ROUTE";

/// Drogon's route macros. They sit inside a class body between
/// `METHOD_LIST_BEGIN` and `METHOD_LIST_END`, which no C++ grammar can parse,
/// so the path is recovered from the text rather than from a node.
const DROGON_ADD_METHOD: [&str; 2] = ["ADD_METHOD_TO", "ADD_METHOD_VIA_REGEX"];

/// The base classes protoc generates for the server side of a service.
const GRPC_SERVICE_BASES: [&str; 3] = ["Service", "AsyncService", "CallbackService"];

/// The generated factory for the client side.
const GRPC_NEW_STUB: &str = "NewStub";

/// The parameter every gRPC handler takes, and the one thing that tells a
/// service method from an ordinary member of the same class.
const GRPC_SERVER_CONTEXT: &str = "ServerContext";

/// The key for a server that registers no route at all.
///
/// libmicrohttpd dispatches by URL inside its handler, so the daemon exposes
/// the whole space and the call says nothing more. `/*` is deliberately not a
/// path any client extractor produces, so this records the surface without
/// inventing a contract that would merge with somebody's `/`.
const WHOLE_SPACE: &str = "/*";

/// Every call, with the callee and the argument list.
const Q_CALL: &str = r#"
(call_expression
  function: (_) @fn
  arguments: (argument_list) @args) @call
"#;

/// Where a name gets its value. Both forms matter: a URL is as often built into
/// a `char[]` declared above as assigned to a `std::string`.
const Q_BINDING: &str = r#"
(init_declarator declarator: (_) @name value: (_) @value)
(assignment_expression left: (_) @name right: (_) @value)
"#;

/// A class or struct with a base list — the gRPC server side.
const Q_RECORD_BASE: &str = r#"
(class_specifier
  name: (type_identifier) @name
  (base_class_clause) @bases
  body: (field_declaration_list) @body)
(struct_specifier
  name: (type_identifier) @name
  (base_class_clause) @bases
  body: (field_declaration_list) @body)
"#;

/// Drogon's macros parse as a declarator with a parameter list, which is where
/// their arguments end up.
const Q_DECLARATOR: &str = r#"
(function_declarator
  declarator: (_) @name
  parameters: (parameter_list) @params)
"#;

macro_rules! cached_c_query {
    ($source:expr) => {{
        static QUERY: OnceLock<Query> = OnceLock::new();
        QUERY.get_or_init(|| {
            Query::new(&tree_sitter_c::LANGUAGE.into(), $source)
                .expect("the query is valid for the c grammar")
        })
    }};
}

macro_rules! cached_cpp_query {
    ($source:expr) => {{
        static QUERY: OnceLock<Query> = OnceLock::new();
        QUERY.get_or_init(|| {
            Query::new(&tree_sitter_cpp::LANGUAGE.into(), $source)
                .expect("the query is valid for the c++ grammar")
        })
    }};
}

/// The compiled query for a pattern that both grammars accept.
///
/// One `static` per macro expansion, so each dialect caches its own compiled
/// query rather than recompiling per file.
fn call_query(dialect: Dialect) -> &'static Query {
    match dialect {
        Dialect::C => cached_c_query!(Q_CALL),
        Dialect::Cpp => cached_cpp_query!(Q_CALL),
    }
}

fn binding_query(dialect: Dialect) -> &'static Query {
    match dialect {
        Dialect::C => cached_c_query!(Q_BINDING),
        Dialect::Cpp => cached_cpp_query!(Q_BINDING),
    }
}

type Caps<'q, 'tree> = std::collections::HashMap<&'q str, Vec<TsNode<'tree>>>;

fn cap<'tree>(caps: &Caps<'_, 'tree>, name: &str) -> Option<TsNode<'tree>> {
    caps.get(name).and_then(|nodes| nodes.first()).copied()
}

/// A node's descendants, itself included.
fn descendants<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        out.push(current);
        stack.extend(ts::children(current));
    }
    out
}

/// The function or lambda a node sits in.
///
/// A URL is built where it is used, so a candidate write from another function
/// is not the one that produced this request.
fn enclosing_function<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(parent.kind(), "function_definition" | "lambda_expression") {
            return Some(parent);
        }
        current = parent.parent();
    }
    None
}

/// Whether the node lies inside the byte range, when there is one.
fn in_range(node: TsNode<'_>, range: Option<(usize, usize)>) -> bool {
    match range {
        Some((start, end)) => node.start_byte() >= start && node.end_byte() <= end,
        None => true,
    }
}

/// The name a write targets: the field of `this->url_`, else the declarator's
/// own identifier.
///
/// The field rather than the whole expression, so a write through `this->url_`
/// and a read of `url_` name the same buffer.
fn bound_name(node: TsNode<'_>, source: &[u8]) -> String {
    if node.kind() == "field_expression"
        && let Some(field) = node.child_by_field_name("field")
    {
        return ts::text(field, source).into_owned();
    }
    let named = ts::first_identifier(node).unwrap_or(node);
    ts::text(named, source).into_owned()
}

/// Characters that may stand between a `%` and the conversion it belongs to:
/// flags, a width, a precision, a length modifier.
const FORMAT_MODIFIERS: &str = "-+ #0123456789.*'hlLqjzt";

/// The conversion characters that end a `printf` specifier.
const FORMAT_CONVERSIONS: &str = "diouxXeEfFgGaAcspn";

/// A `printf` format string as a path pattern.
///
/// `"/engines/%ld"` becomes `/engines/{}`. The whole specifier is swallowed, not
/// just its conversion character: stopping at the first letter would leave the
/// `d` of `%ld` behind and the key would never meet a route.
fn format_to_pattern(format: &str) -> String {
    let mut out = String::with_capacity(format.len());
    let mut chars = format.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }
        while let Some(&next) = chars.peek() {
            chars.next();
            if FORMAT_CONVERSIONS.contains(next) || !FORMAT_MODIFIERS.contains(next) {
                break;
            }
        }
        out.push_str(PLACEHOLDER);
    }
    out
}

/// Whether the text reads as a path or a URL at all.
fn is_path(raw: &str) -> bool {
    raw.starts_with('/') || raw.contains("://")
}

/// Whether a RECONSTRUCTED pattern is worth a boundary key.
///
/// One segment that is neither empty nor a placeholder is the bar: `/engines/{}`
/// clears it, `/{}` and `/` do not. Without the test, every request whose URL is
/// built entirely at runtime would land on one shared BOUNDARY node and link
/// services that have nothing to do with each other. A literal path is held to
/// [`is_path`] instead — `svr.Get("/", handler)` is a real route.
fn usable_path(raw: &str) -> bool {
    if !is_path(raw) {
        return false;
    }
    normalize_http_path(raw)
        .split('/')
        .any(|segment| !segment.is_empty() && segment != PLACEHOLDER)
}

/// The last component of a `::`-qualified name.
fn last_component(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// The receiver's own name, for a member reached through `.`, `->` or `::`.
fn receiver_key(text: &str) -> &str {
    text.rsplit(['.', '>', ':']).next().unwrap_or(text).trim()
}

/// What one write does to a name's value.
enum Written<'tree> {
    /// An initializer or an assignment: the value replaces what was there.
    Expression(TsNode<'tree>),
    /// A literal written into the name by a call — a `printf` format, or the
    /// source of a `strcpy`.
    Pattern(String),
    /// A literal appended by a `strcat`-family call.
    Appended(String),
}

/// One site that gives a name its value.
struct Write<'tree> {
    /// The byte the write sits at: only writes BEFORE a use count, and the last
    /// of those wins.
    at: usize,
    value: Written<'tree>,
}

/// What the second argument of a `verb(path, …)` call says about which side of
/// the contract the call is on.
enum Shape {
    /// A lambda, a function name, an address-of, a bind expression.
    Handler,
    /// A literal, a braced initializer, a number: data for a request.
    Data,
    /// A plain identifier, which either form may pass.
    Ambiguous,
}

struct Extractor<'a, 'tree> {
    source: &'a [u8],
    /// name → every site that writes it, sorted by position.
    writes: BTreeMap<String, Vec<Write<'tree>>>,
}

impl<'a, 'tree> Extractor<'a, 'tree> {
    fn text(&self, node: TsNode<'_>) -> String {
        ts::text(node, self.source).into_owned()
    }

    fn pos(node: TsNode<'_>) -> (u32, u32) {
        let span = ts::span(node);
        (span.start_line, span.start_col)
    }

    /// The content of a string literal, escapes and raw strings included.
    ///
    /// The pieces are concatenated rather than the delimiters trimmed, because
    /// an escape inside a literal is a sibling of the content and trimming a
    /// raw string's `R"(…)"` would need the delimiter's length.
    fn string_content(&self, node: TsNode<'_>) -> String {
        let mut out = String::new();
        for child in ts::children(node) {
            if matches!(
                child.kind(),
                "string_content" | "raw_string_content" | "escape_sequence"
            ) {
                out.push_str(&ts::text(child, self.source));
            }
        }
        out
    }

    /// The node's value when it is a string literal, adjacent literals
    /// included; None when it is anything else.
    fn as_string(&self, node: TsNode<'_>) -> Option<String> {
        match node.kind() {
            "string_literal" | "raw_string_literal" => Some(self.string_content(node)),
            // `"/a" "/b"` is one string in C and C++ alike.
            "concatenated_string" => {
                let mut out = String::new();
                for child in ts::named_children(node) {
                    if matches!(child.kind(), "string_literal" | "raw_string_literal") {
                        out.push_str(&self.string_content(child));
                    } else {
                        return None;
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    fn arguments(&self, args: TsNode<'tree>) -> Vec<TsNode<'tree>> {
        ts::named_children(args)
    }

    /// Records every write a binding performs.
    fn record_bindings(&mut self, bindings: &[Caps<'_, 'tree>]) {
        for caps in bindings {
            let (Some(name), Some(value)) = (cap(caps, "name"), cap(caps, "value")) else {
                continue;
            };
            self.writes
                .entry(bound_name(name, self.source))
                .or_default()
                .push(Write { at: name.start_byte(), value: Written::Expression(value) });
        }
    }

    /// Records the writes a call performs on its first argument.
    ///
    /// The shape is what identifies them: a call whose first argument is a name
    /// and which carries a string literal is writing that literal into the name
    /// — `snprintf(url, sizeof(url), "/engines/%ld", id)`, `strcpy(url, "/x")`.
    /// The format's position differs between `sprintf` and `snprintf`, so the
    /// FIRST string literal after the buffer is taken rather than a fixed index.
    fn record_buffer_writes(&mut self, calls: &[Caps<'_, 'tree>]) {
        for caps in calls {
            let (Some(function), Some(args_node)) = (cap(caps, "fn"), cap(caps, "args")) else {
                continue;
            };
            if function.kind() != "identifier" && function.kind() != "qualified_identifier" {
                continue;
            }
            let args = self.arguments(args_node);
            let Some(target) = args.first() else { continue };
            if !matches!(target.kind(), "identifier" | "field_expression") {
                continue;
            }
            let Some(literal) = args
                .iter()
                .skip(1)
                .find_map(|argument| self.as_string(*argument))
            else {
                continue;
            };
            let pattern = format_to_pattern(&literal);
            let appended = APPEND_FUNCTIONS.contains(&last_component(&self.text(function)));
            let value = if appended {
                Written::Appended(pattern)
            } else {
                Written::Pattern(pattern)
            };
            let name = match target.kind() {
                "field_expression" => target
                    .child_by_field_name("field")
                    .map(|field| self.text(field))
                    .unwrap_or_else(|| self.text(*target)),
                _ => self.text(*target),
            };
            self.writes
                .entry(name)
                .or_default()
                .push(Write { at: target.start_byte(), value });
        }
    }

    /// Puts every name's writes in source order, which is the order they are
    /// replayed in.
    fn sort_writes(&mut self) {
        for writes in self.writes.values_mut() {
            writes.sort_by_key(|write| write.at);
        }
    }

    /// The pattern a name holds at a point in the file.
    ///
    /// Every write before that point, inside the same function, is replayed in
    /// order: an assignment replaces, a `strcat` appends. Writes from another
    /// function are ignored, because two functions in one file routinely build
    /// two different URLs in a buffer they both call `url`.
    fn value_of(
        &self,
        name: &str,
        before: usize,
        scope: Option<(usize, usize)>,
        depth: usize,
    ) -> Option<String> {
        let writes = self.writes.get(name)?;
        let mut current: Option<String> = None;
        for write in writes {
            if write.at >= before {
                break;
            }
            if let Some((start, end)) = scope
                && (write.at < start || write.at >= end)
            {
                continue;
            }
            match &write.value {
                Written::Expression(node) => {
                    current = self.url_expression(*node, write.at, scope, depth + 1);
                }
                Written::Pattern(pattern) => current = Some(pattern.clone()),
                Written::Appended(pattern) => {
                    let mut joined = current.unwrap_or_default();
                    joined.push_str(pattern);
                    current = Some(joined);
                }
            }
        }
        current
    }

    /// A URL expression as a pattern, with everything unrecoverable as `{}`.
    ///
    /// None means nothing at all was recovered — not even one literal — and a
    /// caller must then emit no boundary rather than a key of pure
    /// placeholders.
    fn url_expression(
        &self,
        node: TsNode<'tree>,
        before: usize,
        scope: Option<(usize, usize)>,
        depth: usize,
    ) -> Option<String> {
        if depth > MAX_URL_DEPTH {
            return None;
        }
        match node.kind() {
            "string_literal" | "raw_string_literal" => Some(self.string_content(node)),
            // `"/a" MACRO "/b"`: the macro's expansion is out of reach.
            "concatenated_string" => {
                let mut out = String::new();
                let mut literal = false;
                for child in ts::named_children(node) {
                    if matches!(child.kind(), "string_literal" | "raw_string_literal") {
                        out.push_str(&self.string_content(child));
                        literal = true;
                    } else {
                        out.push_str(PLACEHOLDER);
                    }
                }
                literal.then_some(out)
            }
            // `"/engines/" + std::to_string(ident)`
            "binary_expression" => {
                let operator = node.child_by_field_name("operator")?;
                if self.text(operator).trim() != "+" {
                    return None;
                }
                let left = node.child_by_field_name("left")?;
                let right = node.child_by_field_name("right")?;
                let head = self.url_expression(left, before, scope, depth + 1);
                let tail = self.url_expression(right, before, scope, depth + 1);
                if head.is_none() && tail.is_none() {
                    return None;
                }
                Some(format!(
                    "{}{}",
                    head.unwrap_or_else(|| PLACEHOLDER.to_string()),
                    tail.unwrap_or_else(|| PLACEHOLDER.to_string())
                ))
            }
            "parenthesized_expression" => {
                let inner = ts::named_children(node).into_iter().next()?;
                self.url_expression(inner, before, scope, depth + 1)
            }
            "identifier" => self.value_of(&self.text(node), before, scope, depth + 1),
            // `this->url_`, `state.url`
            "field_expression" => {
                let field = node.child_by_field_name("field")?;
                self.value_of(&self.text(field), before, scope, depth + 1)
            }
            "call_expression" => {
                let function = node.child_by_field_name("function")?;
                // `url.c_str()` is the buffer of `url`.
                if function.kind() == "field_expression"
                    && let Some(field) = function.child_by_field_name("field")
                    && STRING_ACCESSORS.contains(&self.text(field).as_str())
                    && let Some(receiver) = function.child_by_field_name("argument")
                {
                    return self.url_expression(receiver, before, scope, depth + 1);
                }
                // `std::string("/x")`, `strdup(base)`: a one-argument wrapper
                // hands its argument through. `std::to_string(id)` reaches this
                // too and correctly recovers nothing.
                let args = self.arguments(function.parent()?.child_by_field_name("arguments")?);
                if args.len() == 1 {
                    return self.url_expression(args[0], before, scope, depth + 1);
                }
                None
            }
            _ => None,
        }
    }

    fn detail(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn http_ref(
        role: &str,
        verb: &str,
        path: &str,
        confidence: f64,
        framework: &str,
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
            detail: Self::detail(&[
                ("method", verb),
                ("path", &norm),
                ("framework", framework),
            ]),
        }
    }

    // -- libcurl ------------------------------------------------------------

    /// The verb a libcurl request carries.
    ///
    /// GET unless the same function says otherwise. `CURLOPT_CUSTOMREQUEST`
    /// spells the verb out and wins; the rest imply one. An option explicitly
    /// switched off (`CURLOPT_POST, 0L`) implies nothing.
    fn curl_verb(&self, calls: &[Caps<'_, 'tree>], scope: Option<(usize, usize)>) -> String {
        let mut verb = "GET".to_string();
        for caps in calls {
            let Some(args_node) = cap(caps, "args") else { continue };
            if !in_range(args_node, scope) {
                continue;
            }
            let args = self.arguments(args_node);
            let Some(option) = args.get(1).map(|node| self.text(*node)) else {
                continue;
            };
            if option == CURL_CUSTOM_REQUEST {
                if let Some(spelled) = args.get(2).and_then(|node| self.as_string(*node)) {
                    let spelled = spelled.trim();
                    if !spelled.is_empty() {
                        return spelled.to_uppercase();
                    }
                }
                continue;
            }
            let Some((_, implied)) = CURL_VERB_OPTIONS
                .iter()
                .find(|(name, _)| *name == option.as_str())
            else {
                continue;
            };
            let disabled = args
                .get(2)
                .map(|node| self.text(*node))
                .is_some_and(|value| matches!(value.trim_end_matches(['L', 'l']), "0"));
            if !disabled {
                verb = (*implied).to_string();
            }
        }
        verb
    }

    /// Requests made through libcurl.
    fn curl_clients(&self, calls: &[Caps<'_, 'tree>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(call), Some(args_node)) = (cap(caps, "call"), cap(caps, "args")) else {
                continue;
            };
            let args = self.arguments(args_node);
            if args.get(1).map(|node| self.text(*node)).as_deref() != Some(CURL_URL_OPTION) {
                continue;
            }
            let Some(argument) = args.get(CURL_URL_ARGUMENT).copied() else {
                continue;
            };
            let scope = enclosing_function(call).map(|f| (f.start_byte(), f.end_byte()));
            let Some(pattern) =
                self.url_expression(argument, call.start_byte(), scope, 0)
            else {
                continue;
            };
            if !usable_path(&pattern) {
                continue;
            }
            // A literal at the call is worth as much as any other adapter's
            // client URL; one reassembled from a buffer is worth less, and the
            // confidence says which happened.
            let direct = self.as_string(argument).is_some();
            let verb = self.curl_verb(calls, scope);
            refs.push(Self::http_ref(
                "client",
                &verb,
                &pattern,
                if direct { 0.85 } else { 0.7 },
                "libcurl",
                call,
            ));
        }
        refs
    }

    // -- servers written in C ------------------------------------------------

    /// Whether the argument reads as something callable.
    fn is_handler(node: TsNode<'_>) -> bool {
        matches!(
            node.kind(),
            "identifier"
                | "qualified_identifier"
                | "field_expression"
                | "pointer_expression"
                | "lambda_expression"
                | "call_expression"
                | "template_function"
                | "template_method"
        )
    }

    fn shape(node: TsNode<'_>) -> Shape {
        match node.kind() {
            "lambda_expression" | "pointer_expression" | "call_expression"
            | "template_function" | "template_method" => Shape::Handler,
            "string_literal" | "raw_string_literal" | "concatenated_string"
            | "number_literal" | "char_literal" | "initializer_list" | "null"
            | "true" | "false" | "nullptr" => Shape::Data,
            _ => Shape::Ambiguous,
        }
    }

    /// Routes registered through a C library's own API.
    ///
    /// The API name is matched and then VERIFIED by shape: civetweb's
    /// registration binds a handler after a URI, and a call that does not is
    /// not a route however it is named.
    fn c_servers(&self, calls: &[Caps<'_, 'tree>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(function), Some(args_node)) = (cap(caps, "fn"), cap(caps, "args")) else {
                continue;
            };
            if !matches!(function.kind(), "identifier" | "qualified_identifier") {
                continue;
            }
            let callee = self.text(function);
            let args = self.arguments(args_node);

            if last_component(&callee) == CIVETWEB_HANDLER {
                let (Some(path), Some(handler)) = (
                    args.get(1).and_then(|node| self.as_string(*node)),
                    args.get(2).copied(),
                ) else {
                    continue;
                };
                if !Self::is_handler(handler) || !usable_path(&path) {
                    continue;
                }
                refs.push(Self::http_ref("server", "ANY", &path, 0.9, "civetweb", function));
                continue;
            }

            if last_component(&callee) == MICROHTTPD_DAEMON {
                let (line, col) = Self::pos(function);
                let port = args
                    .get(1)
                    .filter(|node| node.kind() == "number_literal")
                    .map(|node| self.text(*node))
                    .unwrap_or_default();
                let key = format!("ANY {WHOLE_SPACE}");
                refs.push(BoundaryRef {
                    mechanism: "http".to_string(),
                    role: "server".to_string(),
                    key: key.clone(),
                    line,
                    col,
                    // The daemon serves HTTP; which routes it serves is decided
                    // inside a handler this cannot read, so the confidence says
                    // the surface is known and the contract is not.
                    confidence: 0.4,
                    detail: Self::detail(&[
                        ("method", "ANY"),
                        ("path", WHOLE_SPACE),
                        ("framework", "microhttpd"),
                        ("port", &port),
                    ]),
                });
            }
        }
        refs
    }

    // -- C++ frameworks -----------------------------------------------------

    /// Routes and requests written as `receiver.Verb(path, …)`.
    ///
    /// cpp-httplib is the library this fits, and the shape generalizes to any
    /// object with verb methods. Which side the call is on is read off the
    /// arguments and never off the receiver: a registration is exactly
    /// `(path, handler)`, so a third argument means a request with a body, a
    /// content type or headers.
    fn verb_methods(&self, calls: &[Caps<'_, 'tree>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(function), Some(args_node)) = (cap(caps, "fn"), cap(caps, "args")) else {
                continue;
            };
            if function.kind() != "field_expression" {
                continue;
            }
            let Some(field) = function.child_by_field_name("field") else {
                continue;
            };
            let verb = self.text(field).to_lowercase();
            if !HTTP_VERBS.contains(&verb.as_str()) {
                continue;
            }
            let args = self.arguments(args_node);
            let Some(path) = args.first().and_then(|node| self.as_string(*node)) else {
                continue;
            };
            if !usable_path(&path) {
                continue;
            }
            let (role, confidence) = match (args.len(), args.get(1).map(|n| Self::shape(*n))) {
                // A route takes the path and the handler and nothing else.
                (2, Some(Shape::Handler)) => ("server", 1.0),
                // `svr.Post("/x", handle_post)` and `cli.Get("/x", headers)`
                // are the same three tokens. A registration is the reading that
                // fits two arguments; a request with data almost always passes
                // a literal or a braced initializer, which `shape` already
                // separated out.
                (2, Some(Shape::Ambiguous)) => ("server", 0.75),
                _ => ("client", 0.85),
            };
            refs.push(Self::http_ref(
                role,
                &verb.to_uppercase(),
                &path,
                confidence,
                "cpp-httplib",
                field,
            ));
        }
        refs
    }

    /// The verbs a Crow route is chained with.
    ///
    /// `CROW_ROUTE(app, "/x").methods("POST"_method)(handler)` puts the verbs
    /// in a call on the macro's RESULT, so they are found by climbing out of
    /// it. A route with no `.methods` answers GET, which is Crow's default.
    fn crow_verbs(&self, call: TsNode<'tree>) -> Vec<String> {
        let mut current = call;
        for _ in 0..MAX_CHAIN_CLIMB {
            let Some(parent) = current.parent() else { break };
            if parent.kind() == "field_expression"
                && let Some(field) = parent.child_by_field_name("field")
                && self.text(field) == "methods"
                && let Some(chained) = parent.parent()
                && let Some(args) = chained.child_by_field_name("arguments")
            {
                let verbs: Vec<String> = self
                    .arguments(args)
                    .into_iter()
                    .filter_map(|argument| self.crow_verb(argument))
                    .collect();
                if !verbs.is_empty() {
                    return verbs;
                }
            }
            current = parent;
        }
        vec!["GET".to_string()]
    }

    /// One verb of a `.methods(...)` list: `"POST"_method` or
    /// `crow::HTTPMethod::Post`.
    fn crow_verb(&self, node: TsNode<'_>) -> Option<String> {
        let text = match node.kind() {
            "user_defined_literal" => ts::child_of_type(node, "string_literal")
                .map(|literal| self.string_content(literal))?,
            "string_literal" => self.string_content(node),
            "qualified_identifier" | "identifier" => {
                last_component(&self.text(node)).to_string()
            }
            _ => return None,
        };
        let verb = text.trim().to_lowercase();
        HTTP_VERBS
            .contains(&verb.as_str())
            .then(|| verb.to_uppercase())
    }

    fn crow_routes(&self, calls: &[Caps<'_, 'tree>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in calls {
            let (Some(call), Some(function), Some(args_node)) =
                (cap(caps, "call"), cap(caps, "fn"), cap(caps, "args"))
            else {
                continue;
            };
            if function.kind() != "identifier" || self.text(function) != CROW_ROUTE {
                continue;
            }
            let args = self.arguments(args_node);
            let Some(path) = args.get(1).and_then(|node| self.as_string(*node)) else {
                continue;
            };
            if !usable_path(&path) {
                continue;
            }
            for verb in self.crow_verbs(call) {
                refs.push(Self::http_ref("server", &verb, &path, 1.0, "crow", function));
            }
        }
        refs
    }

    /// Drogon's `ADD_METHOD_TO(Controller::handler, "/path", Get, Options)`.
    ///
    /// The macro sits between `METHOD_LIST_BEGIN` and `METHOD_LIST_END`, which
    /// is not C++ and which tree-sitter recovers from by wrapping the path in an
    /// ERROR node. So the arguments are read from the source TEXT of the
    /// parameter list: the first quoted run is the path and the words after it
    /// are the verbs. Drogon allows every method when none is named.
    fn drogon_routes(&self, declarators: &[Caps<'_, 'tree>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in declarators {
            let (Some(name), Some(params)) = (cap(caps, "name"), cap(caps, "params")) else {
                continue;
            };
            if !DROGON_ADD_METHOD.contains(&self.text(name).as_str()) {
                continue;
            }
            let text = self.text(params);
            let Some((path, rest)) = split_first_quoted(&text) else {
                continue;
            };
            if !usable_path(path) {
                continue;
            }
            let verbs: Vec<String> = rest
                .split(|ch: char| !ch.is_ascii_alphanumeric())
                .filter(|word| HTTP_VERBS.contains(&word.to_lowercase().as_str()))
                .map(|word| word.to_uppercase())
                .collect();
            let verbs = if verbs.is_empty() {
                vec!["ANY".to_string()]
            } else {
                verbs
            };
            for verb in verbs {
                refs.push(Self::http_ref("server", &verb, path, 0.9, "drogon", name));
            }
        }
        refs
    }

    // -- gRPC ---------------------------------------------------------------

    /// The service a generated base class belongs to.
    ///
    /// `helloworld::Greeter::Service` and `Greeter::AsyncService` both name the
    /// service `Greeter`: it is the component before the generated suffix.
    fn grpc_service_of_base(&self, base: TsNode<'_>) -> Option<String> {
        let text = self.text(base).replace(char::is_whitespace, "");
        let parts: Vec<&str> = text.split("::").collect();
        let [.., service, suffix] = parts.as_slice() else {
            return None;
        };
        if !GRPC_SERVICE_BASES.contains(suffix) || service.is_empty() {
            return None;
        }
        Some((*service).to_string())
    }

    /// The name and parameter list of a member declaration, following the
    /// declarator chain down to the function.
    fn member_signature(
        &self,
        declaration: TsNode<'tree>,
    ) -> Option<(TsNode<'tree>, TsNode<'tree>)> {
        let mut current = declaration.child_by_field_name("declarator")?;
        for _ in 0..8 {
            if current.kind() == "function_declarator" {
                let name = current.child_by_field_name("declarator")?;
                let params = current.child_by_field_name("parameters")?;
                return Some((name, params));
            }
            current = current.child_by_field_name("declarator")?;
        }
        None
    }

    /// A class implementing a gRPC service, one boundary per RPC it handles.
    ///
    /// A member is an RPC handler when it takes a `ServerContext` or overrides
    /// something — the generated base declares every RPC as a virtual method,
    /// and both marks are argument shape rather than a name.
    fn grpc_servers(&self, records: &[Caps<'_, 'tree>]) -> Vec<BoundaryRef> {
        let mut refs = Vec::new();
        for caps in records {
            let (Some(bases), Some(body)) = (cap(caps, "bases"), cap(caps, "body")) else {
                continue;
            };
            let Some(service) = ts::named_children(bases)
                .into_iter()
                .find_map(|base| self.grpc_service_of_base(base))
            else {
                continue;
            };
            for member in ts::named_children(body) {
                if !matches!(member.kind(), "field_declaration" | "function_definition") {
                    continue;
                }
                let Some((name, params)) = self.member_signature(member) else {
                    continue;
                };
                let overrides = ts::children(name.parent().unwrap_or(name))
                    .into_iter()
                    .any(|child| child.kind() == "virtual_specifier");
                if !overrides && !self.text(params).contains(GRPC_SERVER_CONTEXT) {
                    continue;
                }
                let method = self.text(name);
                let (line, col) = Self::pos(name);
                refs.push(BoundaryRef {
                    mechanism: "grpc".to_string(),
                    role: "server".to_string(),
                    key: format!("{service}/{method}"),
                    line,
                    col,
                    confidence: 0.8,
                    detail: Self::detail(&[("service", &service), ("method", &method)]),
                });
            }
        }
        refs
    }

    /// The service of a `X::NewStub(channel)` call inside a value.
    fn grpc_service_of_stub(&self, value: TsNode<'tree>) -> Option<String> {
        for node in descendants(value) {
            if node.kind() != "call_expression" {
                continue;
            }
            let Some(function) = node.child_by_field_name("function") else {
                continue;
            };
            if function.kind() != "qualified_identifier" {
                continue;
            }
            let text = self.text(function).replace(char::is_whitespace, "");
            let parts: Vec<&str> = text.split("::").collect();
            if let [.., service, factory] = parts.as_slice()
                && *factory == GRPC_NEW_STUB
                && !service.is_empty()
            {
                return Some((*service).to_string());
            }
        }
        None
    }

    /// RPCs called on a generated stub.
    ///
    /// The stub's variable is mapped to its service by the value it was given,
    /// so `stub_->SayHello(...)` is an RPC whatever the variable is called.
    fn grpc_clients(
        &self,
        bindings: &[Caps<'_, 'tree>],
        calls: &[Caps<'_, 'tree>],
    ) -> Vec<BoundaryRef> {
        let mut stubs: BTreeMap<String, String> = BTreeMap::new();
        for caps in bindings {
            let (Some(name), Some(value)) = (cap(caps, "name"), cap(caps, "value")) else {
                continue;
            };
            if let Some(service) = self.grpc_service_of_stub(value) {
                stubs.insert(bound_name(name, self.source), service);
            }
        }
        if stubs.is_empty() {
            return Vec::new();
        }

        let mut refs = Vec::new();
        for caps in calls {
            let Some(function) = cap(caps, "fn") else { continue };
            if function.kind() != "field_expression" {
                continue;
            }
            let (Some(receiver), Some(field)) = (
                function.child_by_field_name("argument"),
                function.child_by_field_name("field"),
            ) else {
                continue;
            };
            let receiver = self.text(receiver);
            let Some(service) = stubs
                .get(&receiver)
                .or_else(|| stubs.get(receiver_key(&receiver)))
            else {
                continue;
            };
            let method = self.text(field);
            let (line, col) = Self::pos(field);
            refs.push(BoundaryRef {
                mechanism: "grpc".to_string(),
                role: "client".to_string(),
                key: format!("{service}/{method}"),
                line,
                col,
                confidence: 0.85,
                detail: Self::detail(&[("service", service), ("method", &method)]),
            });
        }
        refs
    }
}

/// The first quoted run of a text, and what follows it.
fn split_first_quoted(text: &str) -> Option<(&str, &str)> {
    let start = text.find('"')?;
    let rest = &text[start + 1..];
    let end = rest.find('"')?;
    Some((&rest[..end], &rest[end + 1..]))
}

/// Every boundary in one file.
///
/// libcurl and the C server libraries are read in both dialects — they are C
/// APIs and a C++ program calls them unchanged. The rest are C++ only.
pub fn extract_boundaries(
    root: TsNode<'_>,
    source: &[u8],
    dialect: Dialect,
) -> Vec<BoundaryRef> {
    let calls = ts::run_query(call_query(dialect), root, source);
    let bindings = ts::run_query(binding_query(dialect), root, source);

    let mut extractor = Extractor { source, writes: BTreeMap::new() };
    extractor.record_bindings(&bindings);
    extractor.record_buffer_writes(&calls);
    extractor.sort_writes();

    let mut refs = extractor.curl_clients(&calls);
    refs.extend(extractor.c_servers(&calls));
    if dialect == Dialect::Cpp {
        refs.extend(extractor.verb_methods(&calls));
        refs.extend(extractor.crow_routes(&calls));
        refs.extend(extractor.drogon_routes(&ts::run_query(
            cached_cpp_query!(Q_DECLARATOR),
            root,
            source,
        )));
        refs.extend(extractor.grpc_servers(&ts::run_query(
            cached_cpp_query!(Q_RECORD_BASE),
            root,
            source,
        )));
        refs.extend(extractor.grpc_clients(&bindings, &calls));
    }
    refs
}

/// Boundaries in a single C source — the entry point for checks.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn extract_c_boundaries(source: &Bound<'_, PyBytes>) -> PyResult<Vec<BoundaryRef>> {
    let source = source.as_bytes();
    let tree = super::parse_tree(source, Dialect::C)?;
    Ok(extract_boundaries(tree.root_node(), source, Dialect::C))
}

/// Boundaries in a single C++ source — the entry point for checks.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn extract_cpp_boundaries(source: &Bound<'_, PyBytes>) -> PyResult<Vec<BoundaryRef>> {
    let source = source.as_bytes();
    let tree = super::parse_tree(source, Dialect::Cpp)?;
    Ok(extract_boundaries(tree.root_node(), source, Dialect::Cpp))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conversion that lets a `printf`-built URL meet a route: every
    /// specifier has to reduce to the same `{}` `normalize_http_path` produces.
    #[test]
    fn a_format_string_becomes_a_path_pattern() {
        assert_eq!(format_to_pattern("/engines/%ld"), "/engines/{}");
        assert_eq!(format_to_pattern("/a/%s/b/%d"), "/a/{}/b/{}");
        assert_eq!(format_to_pattern("/w/%-10.3f"), "/w/{}");
        assert_eq!(format_to_pattern("/x/%zu"), "/x/{}");
        // `%%` is a percent, not a parameter.
        assert_eq!(format_to_pattern("/100%%"), "/100%");
        assert_eq!(format_to_pattern("/plain"), "/plain");
        assert_eq!(format_to_pattern(""), "");
    }

    /// The pattern a format string produces has to survive normalization
    /// unchanged, or the client's key and the server's diverge.
    #[test]
    fn a_pattern_and_a_route_normalize_alike() {
        assert_eq!(
            normalize_http_path(&format_to_pattern("/engines/%ld")),
            normalize_http_path("/engines/{id}")
        );
    }

    #[test]
    fn only_a_path_with_a_real_segment_is_usable() {
        assert!(usable_path("/engines/{}"));
        assert!(usable_path("https://api.example.com/v1/things"));
        assert!(usable_path("/a"));
        // Nothing was recovered but the shape.
        assert!(!usable_path("/{}"));
        assert!(!usable_path("/"));
        assert!(!usable_path("{}"));
        assert!(!usable_path(""));
        // A bare word is not a path.
        assert!(!usable_path("url"));
    }

    #[test]
    fn a_qualified_name_reduces_to_its_last_component() {
        assert_eq!(last_component("std::snprintf"), "snprintf");
        assert_eq!(last_component("snprintf"), "snprintf");
        assert_eq!(receiver_key("this->stub_"), "stub_");
        assert_eq!(receiver_key("state.client"), "client");
        assert_eq!(receiver_key("stub"), "stub");
    }

    #[test]
    fn the_first_quoted_run_of_a_drogon_macro_is_its_path() {
        assert_eq!(
            split_first_quoted("Ctrl::getOne, \"/engines/{1}\", Get"),
            Some(("/engines/{1}", ", Get"))
        );
        assert_eq!(split_first_quoted("Ctrl::getOne, Get"), None);
    }
}
