//! Structural extraction from Rust: modules, types, functions, imports.
//!
//! Unlike Go, `impl` blocks are dispatched in a second pass. Rust does not
//! require a type or trait to be declared above its `impl`, and without
//! deferring, an `impl Trait for Type` written above `struct Type` would find
//! no type node and lose the `base` occurrence.
//!
//! Every type name under a type (a generic argument, an `impl` self type, a bound,
//! a `where` clause, a `let`, alias, cast or closure type, a tuple field) is an
//! `annotation` occurrence. The header of an `impl` is enclosed by the FILE node,
//! never the MODULE: a Rust MODULE has no `file_path`, so an edge from it could not
//! be placed by the LSIF/SCIP exporters.
//!
//! Every segment of a path before the last (`Foo` in `Foo::new()`, `inner`, `crate`,
//! `super`, the `crate` of `pub(crate)`) is a `read` occurrence, never an annotation:
//! a module is not a type. `self` and primitive qualifiers are not recorded.
//!
//! A name in expression or pattern position is a `read` (a `write` for an assignment
//! target) with `detail` `value`: a const or static, the last segment of a path
//! (`Mode::A`), a struct literal's type and its field initializers (shorthand
//! included), and the paths and fields of struct and tuple-struct patterns. A
//! lowercase identifier in a pattern is a binding and is not recorded. Callees and
//! plain-name arguments keep their own occurrence: `collect_calls` marks them in
//! `recorded`.
//!
//! Enum variants (a CLASS `Enum.Variant`, called like a tuple struct; the fields of a
//! struct variant are ATTRIBUTEs), the associated types (TYPE_ALIAS) and consts
//! (VARIABLE) of impls and traits, and every item written inside a function body
//! (qualified under the function, `m::f::Local`) are declared. A const or static
//! initializer is scanned like a body: its calls and reads are enclosed by the const.

use std::collections::{HashMap, HashSet};

use crate::py::prelude::*;
use crate::py::types::{PyDict, PyList};
use tree_sitter::Node as TsNode;

use crate::graph::Graph;
use crate::ids::node_id;
use crate::node::{Node, NodeKind};
use crate::occurrence::OccurrenceRef;
use crate::relation::{Relation, RelationKind};
use crate::ts;

/// The node itself, or every descendant of the given kind; we do not descend
/// into a match. The node is tested first because a bare type name (`arg: Foo`,
/// `-> Foo`, `-> Self`) is itself a childless `type_identifier`.
fn walk_type<'tree>(node: TsNode<'tree>, kind: &str) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == kind {
            out.push(current);
        } else {
            stack.extend(ts::children(current));
        }
    }
    out
}

/// The leftmost node of a set — the leading name of a compound type.
fn leftmost<'tree>(nodes: &[TsNode<'tree>]) -> Option<TsNode<'tree>> {
    nodes.iter().min_by_key(|n| n.start_byte()).copied()
}

/// Whether the node is the left-hand side of an assignment.
fn is_assignment_target(node: TsNode<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(parent.kind(), "assignment_expression" | "compound_assignment_expr")
            && parent.child_by_field_name("left") == Some(node)
    })
}

/// The name a path or a field access ends in - the position the resolver is
/// asked about - when `node` is a name, a path, a field or a turbofish.
fn plain_name<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    match node.kind() {
        "identifier" => Some(node),
        "scoped_identifier" => node.child_by_field_name("name"),
        "field_expression" => node.child_by_field_name("field"),
        "generic_function" => node.child_by_field_name("function").and_then(plain_name),
        "parenthesized_expression" => ts::named_children(node).first().copied().and_then(plain_name),
        _ => None,
    }
}

/// The name a call's callee ends in.
fn callee_name<'tree>(call: TsNode<'tree>) -> Option<TsNode<'tree>> {
    call.child_by_field_name("function").and_then(plain_name)
}

/// The last segment of a path that names a type, a struct or a tuple-struct pattern's
/// constructor: `Cfg`, `m::Cfg`, `Cfg::<T>`.
fn last_segment<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    match node.kind() {
        "identifier" | "type_identifier" => Some(node),
        "scoped_identifier" | "scoped_type_identifier" => node
            .child_by_field_name("name")
            .filter(|name| matches!(name.kind(), "identifier" | "type_identifier")),
        "generic_type" | "generic_type_with_turbofish" => {
            node.child_by_field_name("type").and_then(last_segment)
        }
        _ => None,
    }
}

/// Subtrees the value pass never enters: types (they belong to annotations), items
/// declared inside a body, `use`, macros the project defines or that are opaque,
/// attributes, lifetimes and labels.
fn skips_values(kind: &str) -> bool {
    kind.ends_with("_type")
        || matches!(
            kind,
            "type_identifier"
                | "scoped_type_identifier"
                | "generic_type_with_turbofish"
                | "type_arguments"
                | "function_item"
                | "use_declaration"
                | "macro_definition"
                | "macro_invocation"
                | "attribute_item"
                | "inner_attribute_item"
                | "visibility_modifier"
                | "lifetime"
                | "label"
                | "impl_item"
                | "trait_item"
                | "struct_item"
                | "enum_item"
                | "union_item"
                | "mod_item"
                | "type_item"
                | "extern_crate_declaration"
                | "foreign_mod_item"
                | "const_item"
                | "static_item"
        )
}

/// Whether the identifier is the bare receiver of a field access (`x` in `x.f`).
fn is_field_receiver(node: TsNode<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "field_expression" && parent.child_by_field_name("value") == Some(node)
    })
}

/// Primitive type names: `u8::MAX` and `str::len` qualify with a type rust-analyzer
/// has no definition for, so no query is made for them.
const PRIMITIVES: [&str; 17] = [
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32",
    "i64", "i128", "isize", "f32", "f64",
];

/// The segment a path's `path` child ends in, when it is a name rust-analyzer can answer.
fn prefix_segment<'tree>(path: TsNode<'tree>, source: &[u8]) -> Option<TsNode<'tree>> {
    match path.kind() {
        "identifier" | "type_identifier" => {
            let name = ts::text(path, source);
            if PRIMITIVES.contains(&name.as_ref()) {
                None
            } else {
                Some(path)
            }
        }
        "crate" | "super" => Some(path),
        "scoped_identifier" | "scoped_type_identifier" => path.child_by_field_name("name"),
        "generic_type" | "generic_type_with_turbofish" => path
            .child_by_field_name("type")
            .and_then(|inner| prefix_segment(inner, source)),
        // `self`, `<T as Tr>::`, a macro metavariable: no answer.
        _ => None,
    }
}

/// The last segment of the `path` of every `scoped_identifier` / `scoped_type_identifier`
/// at or under `node` (an outer path's own prefix is found when the walk reaches the
/// inner path). Sorted by start byte, without repeats. The walk does not enter nested
/// functions (they are nodes of their own), `use` declarations, visibility modifiers
/// or `macro_rules!` definitions.
fn path_prefix_segments<'tree>(node: TsNode<'tree>, source: &[u8]) -> Vec<TsNode<'tree>> {
    let mut out: Vec<TsNode<'tree>> = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current != node
            && (is_body_item(current.kind())
                || matches!(
                    current.kind(),
                    "use_declaration" | "visibility_modifier" | "macro_definition"
                ))
        {
            continue;
        }
        if matches!(current.kind(), "scoped_identifier" | "scoped_type_identifier")
            && let Some(path) = current.child_by_field_name("path")
            && let Some(segment) = prefix_segment(path, source)
        {
            out.push(segment);
        }
        stack.extend(ts::children(current));
    }
    out.sort_by_key(|n| n.start_byte());
    out.dedup_by_key(|n| n.start_byte());
    out
}

/// Items that become nodes of their own wherever they are written, so a scope that
/// contains one does not own what the item calls or reads.
fn is_body_item(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "const_item"
            | "static_item"
            | "struct_item"
            | "enum_item"
            | "union_item"
            | "trait_item"
            | "impl_item"
            | "type_item"
            | "macro_definition"
    )
}

/// A scope's descendants, not entering the items declared inside it: those
/// are nodes of their own and own what they call.
fn owned_descendants<'tree>(scope: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![scope];
    while let Some(current) = stack.pop() {
        out.push(current);
        for child in ts::children(current) {
            if !is_body_item(child.kind()) {
                stack.push(child);
            }
        }
    }
    out
}

/// A node's descendants, not entering the functions inside it. Wider than
/// `owned_descendants`: `extract` uses it to find every `macro_rules!`, which may sit
/// inside a `const _: () = { ... }` block.
fn descendants_outside_functions<'tree>(scope: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![scope];
    while let Some(current) = stack.pop() {
        out.push(current);
        for child in ts::children(current) {
            if child.kind() != "function_item" {
                stack.push(child);
            }
        }
    }
    out
}

/// The items declared directly in a scope (a function body), not those inside them.
/// The walk passes through everything else, a `mod` included.
fn body_items<'tree>(scope: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = ts::children(scope);
    while let Some(current) = stack.pop() {
        if is_body_item(current.kind()) {
            out.push(current);
        } else {
            stack.extend(ts::children(current));
        }
    }
    out.reverse();
    out
}

/// A name a `use` declaration brings in (one per leaf of the use tree), or a bodyless
/// `mod x;`. The visitor only reports it; the adapter binds it by path in its structure
/// pass and sends what it could not bind to the resolver.
pub struct UseSite {
    /// Role `import` enclosed by the IMPORT node (a leaf), or role `read` enclosed by the
    /// FILE (a `mod x;`).
    pub occurrence: OccurrenceRef,
    /// The leaf's full path (`crate::shapes::Circle`, whitespace removed), or the module's name.
    pub path: String,
    /// The module the declaration sits in.
    pub module_qname: String,
    /// `classify_import` of `path`.
    pub origin: &'static str,
    /// True for `mod x;`.
    pub mod_decl: bool,
}

/// What `RustExtractor::into_parts` hands over: occurrences, internal imports, inline
/// modules and use sites.
pub type ExtractorParts = (
    Vec<OccurrenceRef>,
    Vec<(String, String, String, String)>,
    Vec<(String, crate::span::Span)>,
    Vec<UseSite>,
);

/// The text with every whitespace character removed.
fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// `prefix::tail`, without a leading `::` when the prefix is empty.
fn join_use_path(prefix: &str, tail: &str) -> String {
    if prefix.is_empty() {
        tail.to_string()
    } else if tail.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}::{tail}")
    }
}

pub struct RustExtractor<'a> {
    py: Python<'a>,
    graph: &'a mut Graph,
    source: &'a [u8],
    project_name: &'a str,
    /// Changed for the duration of a `mod foo { ... }` walk.
    module_qname: String,
    file_id: &'a str,
    file_rel: &'a str,
    crate_name: Option<&'a str>,
    deps: &'a HashSet<String>,
    occurrences: Vec<OccurrenceRef>,
    /// `(import node id, path, importing module, importing file)` for internal
    /// imports: the adapter substitutes the real MODULE node once every module
    /// exists, and emits IMPORTS from the file at the same time.
    internal_imports: Vec<(String, String, String, String)>,
    /// The `macro_rules!` of this file, by name.
    macros: HashMap<String, super::macros::MacroRules>,
    /// Start byte of every identifier already recorded as an annotation, an impl's `base`, a path prefix, a callee, a call argument or a value: one site never yields two occurrences.
    recorded: HashSet<usize>,
    /// `(qualified name, name span)` of every inline `mod foo { ... }` with a body.
    inline_modules: Vec<(String, crate::span::Span)>,
    /// Every name a `use` brings in and every bodyless `mod x;`, in source order.
    use_sites: Vec<UseSite>,
    /// The names of every `macro_rules!` of the crate (all files), so an invocation of a
    /// macro written in another file is still named.
    project_macros: HashSet<String>,
    /// Node id -> the name span of every declaration of that node seen in THIS file, in
    /// source order; the first entry is the node's own `name_span`. Only these ids may get
    /// `declared_at`: `SpanIndex` keys `declared_at` spans by the node's own `file_path`, so
    /// a span of another file (two files sharing a module name through `#[path]`) would be
    /// indexed in the wrong file and is left out.
    declared_here: HashMap<String, Vec<crate::span::Span>>,
}

impl<'a> RustExtractor<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        py: Python<'a>,
        graph: &'a mut Graph,
        source: &'a [u8],
        project_name: &'a str,
        module_qname: &str,
        file_id: &'a str,
        file_rel: &'a str,
        crate_name: Option<&'a str>,
        deps: &'a HashSet<String>,
    ) -> Self {
        Self {
            py,
            graph,
            source,
            project_name,
            module_qname: module_qname.to_string(),
            file_id,
            file_rel,
            crate_name,
            deps,
            occurrences: Vec::new(),
            internal_imports: Vec::new(),
            macros: HashMap::new(),
            recorded: HashSet::new(),
            inline_modules: Vec::new(),
            use_sites: Vec::new(),
            project_macros: HashSet::new(),
            declared_here: HashMap::new(),
        }
    }

    /// The `macro_rules!` names of the whole crate.
    pub fn with_project_macros(mut self, names: &HashSet<String>) -> Self {
        self.project_macros = names.clone();
        self
    }

    pub fn into_parts(self) -> ExtractorParts {
        (self.occurrences, self.internal_imports, self.inline_modules, self.use_sites)
    }

    fn text(&self, node: TsNode<'_>) -> String {
        ts::text(node, self.source).into_owned()
    }

    fn text_or_empty(&self, node: Option<TsNode<'_>>) -> String {
        node.map(|n| self.text(n)).unwrap_or_default()
    }

    pub fn extract(&mut self, root: TsNode<'_>) -> PyResult<()> {
        for node in descendants_outside_functions(root) {
            if node.kind() == "macro_definition"
                && let Some((name, rules)) = super::macros::parse_definition(node, self.source)
            {
                self.macros.insert(name, rules);
            }
        }
        self.dispatch(root)
    }

    /// Dispatches direct children to their handlers; `impl` in a second pass.
    fn dispatch(&mut self, node: TsNode<'_>) -> PyResult<()> {
        self.dispatch_items(ts::children(node))
    }

    /// Dispatches a list of items; `impl` in a second pass.
    fn dispatch_items(&mut self, items: Vec<TsNode<'_>>) -> PyResult<()> {
        let mut deferred_impls = Vec::new();
        for child in items {
            match child.kind() {
                "impl_item" => deferred_impls.push(child),
                "mod_item" => self.on_mod_item(child)?,
                "function_item" => self.on_function_item(child)?,
                "struct_item" | "enum_item" | "trait_item" | "union_item" => {
                    if let Some(id) = self.declare_named(child, NodeKind::Class)?
                        && let Some(name_node) = child.child_by_field_name("name")
                    {
                        let qname = format!("{}::{}", self.module_qname, self.text(name_node));
                        self.record_generics(child, &id);
                        self.collect_fields(child, &qname, &id)?;
                        if child.kind() == "trait_item" {
                            self.collect_trait_methods(child, &qname, &id)?;
                        }
                    }
                }
                "type_item" => {
                    if let Some(id) = self.declare_named(child, NodeKind::TypeAlias)? {
                        self.record_generics(child, &id);
                        if let Some(type_node) = child.child_by_field_name("type") {
                            self.record_annotation(type_node, &id);
                        }
                    }
                }
                "const_item" | "static_item" => {
                    if let Some(id) = self.declare_named(child, NodeKind::Variable)? {
                        self.scan_value(child, &id);
                    }
                }
                "use_declaration" => self.on_use_declaration(child)?,
                "macro_definition" => self.on_macro_definition(child)?,
                "macro_invocation" => {
                    // Enclosed by the FILE: a Rust MODULE has no file_path.
                    let file_id = self.file_id;
                    self.on_project_macro(child, file_id);
                }
                // `make!();` at item level is an `expression_statement` wrapping the
                // `macro_invocation` (the `;` belongs to the statement).
                "expression_statement" => {
                    if let Some(invocation) = ts::child_of_type(child, "macro_invocation") {
                        let file_id = self.file_id;
                        self.on_project_macro(invocation, file_id);
                    }
                }
                _ => {}
            }
        }
        for child in deferred_impls {
            self.on_impl_item(child)?;
        }
        Ok(())
    }

    /// Descends into `mod foo { ... }` with a nested module name.
    ///
    /// Without this, everything inside an inline module (idiomatic Rust, e.g.
    /// `#[cfg(test)] mod tests { ... }`) would be silently dropped. A bodyless
    /// `mod foo;` has nothing to walk.
    /// An inline module is reported through `into_parts`, so the adapter can make its MODULE.
    fn on_mod_item(&mut self, node: TsNode<'_>) -> PyResult<()> {
        // A Rust MODULE has no file_path: the visibility is enclosed by the FILE.
        let file_id = self.file_id;
        self.record_visibility(node, file_id);
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let Some(body) = node.child_by_field_name("body") else {
            // `mod x;`: a read of the module at its name, enclosed by the FILE like the
            // visibility above (a Rust MODULE has no file). The adapter binds it.
            let span = ts::span(name_node);
            self.use_sites.push(UseSite {
                occurrence: OccurrenceRef {
                    role: "read".to_string(),
                    line: span.start_line,
                    col: span.start_col,
                    enclosing_id: file_id.to_string(),
                    span,
                    detail: None,
                },
                path: self.text(name_node),
                module_qname: self.module_qname.clone(),
                origin: "internal",
                mod_decl: true,
            });
            return Ok(());
        };
        let inner = format!("{}::{}", self.module_qname, self.text(name_node));
        self.inline_modules.push((inner.clone(), ts::span(name_node)));
        let outer = std::mem::replace(&mut self.module_qname, inner);
        let result = self.dispatch(body);
        self.module_qname = outer;
        result
    }

    /// One node per qualified name. A qualified name carries no trait and no cfg, so
    /// same-named methods of different impls of one type (`impl Add<S> for S` and
    /// `impl Add<&S> for S`, an inherent and a trait method, cfg variants) and same-named
    /// items of one scope are ONE node. Every later declaration of it in the same file adds
    /// its name span to `metadata["declared_at"]` (read by `SpanIndex::from_graph`), so a
    /// resolver that answers at the second declaration still binds to the node.
    fn declare(
        &mut self,
        qname: &str,
        name: &str,
        kind: NodeKind,
        full_node: TsNode<'_>,
        name_node: TsNode<'_>,
    ) -> PyResult<String> {
        let id = node_id(self.project_name, qname, kind.as_str());
        // Per site, not per node: a second declaration still has its own visibility.
        self.record_visibility(full_node, &id);
        let name_span = ts::span(name_node);
        if self.graph.has_node(&id) {
            // Only a node this file declared: see `declared_here`.
            let extra: Vec<crate::span::Span> = match self.declared_here.get_mut(&id) {
                Some(spans) if !spans.contains(&name_span) => {
                    spans.push(name_span);
                    spans[1..].to_vec()
                }
                _ => return Ok(id),
            };
            if let Some(node) = self.graph.node_map().get(&id) {
                node.get()
                    .metadata
                    .bind(self.py)
                    .set_item("declared_at", PyList::new(self.py, extra)?)?;
            }
            return Ok(id);
        }
        self.declared_here.insert(id.clone(), vec![name_span]);
        let metadata = PyDict::new(self.py);
        metadata.set_item("name_span", name_span)?;
        // What the author wrote above it.
        if let Some(doc) = crate::doc::leading_comment(full_node, self.source) {
            metadata.set_item("doc", doc)?;
        }
        crate::doc::add_comments(self.py, &metadata, full_node, self.source)?;
        let node = Node {
            id: id.clone(),
            kind,
            qualified_name: qname.to_string(),
            name: name.to_string(),
            file_path: Some(self.file_rel.to_string()),
            span: Some(ts::span(full_node)),
            metadata: metadata.unbind(),
        };
        self.graph.insert_node(id.clone(), Py::new(self.py, node)?);
        self.push_relation(self.file_id.to_string(), id.clone(), RelationKind::Declares)?;
        Ok(id)
    }

    fn declare_named(&mut self, node: TsNode<'_>, kind: NodeKind) -> PyResult<Option<String>> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(None);
        };
        let name = self.text(name_node);
        let qname = format!("{}::{name}", self.module_qname);
        self.declare(&qname, &name, kind, node, name_node).map(Some)
    }

    fn add_occurrence(&mut self, role: &str, name_node: TsNode<'_>, enclosing_id: &str) {
        self.add_occurrence_with(role, name_node, enclosing_id, None);
    }

    fn add_occurrence_with(
        &mut self,
        role: &str,
        name_node: TsNode<'_>,
        enclosing_id: &str,
        detail: Option<String>,
    ) {
        let span = ts::span(name_node);
        self.occurrences.push(OccurrenceRef {
            role: role.to_string(),
            line: span.start_line,
            col: span.start_col,
            enclosing_id: enclosing_id.to_string(),
            span,
            detail,
        });
    }

    /// A `read` for every segment of a path before the last one, under `scope`, once
    /// per site. A qualifier is never an `annotation`: a module is not a type.
    fn record_path_prefixes(&mut self, scope: TsNode<'_>, enclosing_id: &str) {
        for segment in path_prefix_segments(scope, self.source) {
            if self.recorded.insert(segment.start_byte()) {
                self.add_occurrence("read", segment, enclosing_id);
            }
        }
    }

    /// A `read` for the modules a visibility names: `crate` / `super` of `pub(crate)` /
    /// `pub(super)`, and every segment of `pub(in path)`. `pub(self)` names nothing.
    fn record_visibility_modifier(&mut self, modifier: TsNode<'_>, enclosing_id: &str) {
        let mut segments: Vec<TsNode<'_>> = Vec::new();
        for child in ts::children(modifier) {
            match child.kind() {
                "crate" | "super" | "identifier" => segments.push(child),
                "scoped_identifier" => {
                    segments.extend(path_prefix_segments(child, self.source));
                    if let Some(name) = child.child_by_field_name("name") {
                        segments.push(name);
                    }
                }
                _ => {}
            }
        }
        for segment in segments {
            if self.recorded.insert(segment.start_byte()) {
                self.add_occurrence("read", segment, enclosing_id);
            }
        }
    }

    /// The visibility of a declaration, if it has one.
    fn record_visibility(&mut self, full_node: TsNode<'_>, enclosing_id: &str) {
        if let Some(modifier) = ts::child_of_type(full_node, "visibility_modifier") {
            self.record_visibility_modifier(modifier, enclosing_id);
        }
    }

    /// A `read` (a `write` for an assignment target) with `detail` `value` for every name
    /// in expression or pattern position of a callable: its parameter patterns and its
    /// body. Must run after `collect_calls` and `record_path_prefixes`, which mark the
    /// callee names, the plain-name arguments and the path prefixes in `recorded`.
    fn collect_values(&mut self, callable: TsNode<'_>, enclosing_id: &str) {
        if let Some(parameters) = callable.child_by_field_name("parameters") {
            for parameter in ts::named_children(parameters) {
                if parameter.kind() == "parameter" {
                    self.walk_values(parameter, false, enclosing_id);
                }
            }
        }
        if let Some(body) = callable.child_by_field_name("body") {
            self.walk_values(body, false, enclosing_id);
        }
    }

    /// One occurrence for `name`, once per site. `site` is the node whose role matters
    /// (the whole path for a `scoped_identifier`).
    fn record_value(&mut self, site: TsNode<'_>, name: TsNode<'_>, enclosing_id: &str) {
        if self.recorded.insert(name.start_byte()) {
            let role = if is_assignment_target(site) { "write" } else { "read" };
            self.add_occurrence_with(role, name, enclosing_id, Some(super::VALUE_DETAIL.to_string()));
        }
    }

    /// The walk behind `collect_values`. `pattern` is true in pattern position, where a
    /// lowercase identifier is a binding and an uppercase one a const, unit struct or variant.
    fn walk_values(&mut self, node: TsNode<'_>, pattern: bool, enclosing_id: &str) {
        let kind = node.kind();
        if skips_values(kind) {
            return;
        }
        match kind {
            "identifier" => {
                if pattern {
                    if self.text(node).starts_with(|c: char| c.is_uppercase()) {
                        self.record_value(node, node, enclosing_id);
                    }
                } else if !is_field_receiver(node) {
                    self.record_value(node, node, enclosing_id);
                }
            }
            // Only the outermost path is reached (the walk does not enter it): its last
            // segment is the value, its prefixes are recorded already.
            "scoped_identifier" => {
                if let Some(name) = node.child_by_field_name("name")
                    && name.kind() == "identifier"
                {
                    self.record_value(node, name, enclosing_id);
                }
            }
            "field_expression" => {
                // `x.f`: the bare receiver is not recorded; anything else is walked.
                if let Some(value) = node.child_by_field_name("value")
                    && value.kind() != "identifier"
                {
                    self.walk_values(value, false, enclosing_id);
                }
            }
            "struct_expression" => {
                if let Some(name) = node.child_by_field_name("name").and_then(last_segment) {
                    self.record_value(name, name, enclosing_id);
                }
                let Some(body) = node.child_by_field_name("body") else {
                    return;
                };
                for item in ts::named_children(body) {
                    match item.kind() {
                        "field_initializer" => {
                            if let Some(field) = item.child_by_field_name("field")
                                && field.kind() == "field_identifier"
                            {
                                self.record_value(field, field, enclosing_id);
                            }
                            if let Some(value) = item.child_by_field_name("value") {
                                self.walk_values(value, false, enclosing_id);
                            }
                        }
                        "shorthand_field_initializer" => {
                            if let Some(name) = ts::child_of_type(item, "identifier") {
                                self.record_value(name, name, enclosing_id);
                            }
                        }
                        // `..base`
                        _ => self.walk_values(item, false, enclosing_id),
                    }
                }
            }
            "let_declaration" | "let_condition" | "for_expression" => {
                let target = node.child_by_field_name("pattern");
                for child in ts::named_children(node) {
                    self.walk_values(child, Some(child) == target, enclosing_id);
                }
            }
            "match_pattern" => {
                // The pattern is a pattern; the `if` guard is an expression.
                let guard = node.child_by_field_name("condition");
                for child in ts::named_children(node) {
                    self.walk_values(child, Some(child) != guard, enclosing_id);
                }
            }
            "closure_parameters" => {
                for child in ts::named_children(node) {
                    self.walk_values(child, true, enclosing_id);
                }
            }
            "parameter" => {
                if let Some(inner) = node.child_by_field_name("pattern") {
                    self.walk_values(inner, true, enclosing_id);
                }
            }
            "tuple_struct_pattern" => {
                let constructor = node.child_by_field_name("type");
                if let Some(name) = constructor.and_then(last_segment) {
                    self.record_value(name, name, enclosing_id);
                }
                for child in ts::named_children(node) {
                    if Some(child) != constructor {
                        self.walk_values(child, true, enclosing_id);
                    }
                }
            }
            "struct_pattern" => {
                if let Some(name) = node.child_by_field_name("type").and_then(last_segment) {
                    self.record_value(name, name, enclosing_id);
                }
                for field in ts::named_children(node) {
                    if field.kind() != "field_pattern" {
                        continue;
                    }
                    if let Some(name) = field.child_by_field_name("name") {
                        self.record_value(name, name, enclosing_id);
                    }
                    if let Some(inner) = field.child_by_field_name("pattern") {
                        self.walk_values(inner, true, enclosing_id);
                    }
                }
            }
            _ => {
                for child in ts::named_children(node) {
                    self.walk_values(child, pattern, enclosing_id);
                }
            }
        }
    }

    /// A call site for every `call_expression` inside the scope.
    fn collect_calls(&mut self, scope: TsNode<'_>, enclosing_id: &str) {
        for node in owned_descendants(scope) {
            // `call_all!(a, b)` where the project defines the macro: the name is a call,
            // and what it expands to is what is called.
            if node.kind() == "macro_invocation" {
                self.on_project_macro(node, enclosing_id);
                continue;
            }
            if node.kind() != "call_expression" {
                continue;
            }
            let Some(function) = node.child_by_field_name("function") else {
                continue;
            };
            let mut site = None;
            let mut callee: Option<String> = None;
            if let Some(name_node) = plain_name(function) {
                self.add_occurrence("call", name_node, enclosing_id);
                self.recorded.insert(name_node.start_byte());
                site = Some(ts::span(name_node));
                callee = Some(self.text(name_node));
            } else if function.kind() == "call_expression" {
                // `f()()`: a call of whatever `f` returns.
                if let Some(inner) = callee_name(function) {
                    self.add_occurrence_with("call", inner, enclosing_id, Some("result".into()));
                    self.recorded.insert(inner.start_byte());
                }
            } else if function.kind() == "index_expression"
                && let Some(base) = ts::named_children(function).first().copied().and_then(plain_name)
            {
                // `table[i]()`: a call of whatever the container holds.
                self.add_occurrence("call", base, enclosing_id);
                self.recorded.insert(base.start_byte());
            }
            // The names handed to the call say which parameter they are handed to.
            if let (Some(site), Some(arguments)) = (site, node.child_by_field_name("arguments")) {
                for (index, argument) in ts::named_children(arguments).into_iter().enumerate() {
                    if let Some(name) = plain_name(argument) {
                        let detail = format!("arg:{}:{}:{index}", site.start_line, site.start_col);
                        self.add_occurrence_with("read", name, enclosing_id, Some(detail));
                        self.recorded.insert(name.start_byte());
                        // `iter.map(f)`, `thread::spawn(f)`: the library calls what it is given.
                        if callee.as_deref().is_some_and(calls_its_arguments) {
                            let detail = format!("c:{}:{}", site.start_line, site.start_col);
                            self.add_occurrence_with("callarg", name, enclosing_id, Some(detail));
                        }
                    }
                }
            }
        }
    }

    /// `macro_rules! name { .. }` is a FUNCTION `module::name` with `metadata["macro"] =
    /// "macro_rules"`, declared like a free function (name span, doc, DECLARES from the
    /// FILE; no CONTAINS, as for functions). The expansion table is built in `extract`.
    fn on_macro_definition(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = node
            .child_by_field_name("name")
            .or_else(|| ts::child_of_type(node, "identifier"))
        else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}::{name}", self.module_qname);
        let id = node_id(self.project_name, &qname, NodeKind::Function.as_str());
        let existed = self.graph.has_node(&id);
        self.declare(&qname, &name, NodeKind::Function, node, name_node)?;
        if !existed && let Some(created) = self.graph.node_map().get(&id) {
            created.get().metadata.bind(self.py).set_item("macro", "macro_rules")?;
        }
        Ok(())
    }

    /// An invocation `name!(..)`: when `name` is a macro of the project (any file) a
    /// `call` occurrence sits on the name (the last path segment of `crate::name!`), and
    /// when the macro is of this file its expansion's calls are recorded too.
    fn on_project_macro(&mut self, node: TsNode<'_>, enclosing_id: &str) {
        let Some(name) = node.child_by_field_name("macro") else {
            return;
        };
        let text = self.text(name);
        let last = text.rsplit("::").next().unwrap_or(&text).trim().to_string();
        if self.project_macros.contains(&last) {
            let segment = match name.kind() {
                "identifier" => Some(name),
                "scoped_identifier" => name.child_by_field_name("name"),
                _ => None,
            };
            if let Some(segment) = segment
                && self.recorded.insert(segment.start_byte())
            {
                self.add_occurrence("call", segment, enclosing_id);
            }
        }
        if self.macros.contains_key(&last)
            && let Some(arguments) = ts::child_of_type(node, "token_tree")
        {
            let mut tokens = Vec::new();
            super::macros::flatten(arguments, self.source, true, &mut tokens);
            // The outer delimiters are not arguments.
            if tokens.len() >= 2 {
                tokens.remove(0);
                tokens.pop();
            }
            let mut seen = HashSet::new();
            self.expand_invocation(&last, &tokens, enclosing_id, 0, &mut seen);
        }
    }

    /// The calls a project macro makes where it is invoked. A callee an argument
    /// supplied is placed where the invocation wrote it, where the resolver can
    /// answer for it; one the macro's body wrote is found by its name.
    fn expand_invocation(
        &mut self,
        name: &str,
        arguments: &[super::macros::Tok],
        enclosing_id: &str,
        depth: usize,
        seen: &mut HashSet<(u32, u32, String)>,
    ) {
        if depth > 4 {
            return;
        }
        let Some(rules) = self.macros.get(name) else {
            return;
        };
        let Some(tokens) = super::macros::expand(rules, arguments) else {
            return;
        };
        let mut calls: Vec<(super::macros::Tok, bool)> = Vec::new();
        let mut nested: Vec<(String, Vec<super::macros::Tok>)> = Vec::new();
        for found in super::macros::scan(&tokens) {
            match found {
                super::macros::Found::Call(token) => calls.push((token.clone(), token.from_args)),
                super::macros::Found::Macro(token, inner) => {
                    // A macro the invocation's own arguments name has a real position.
                    if token.from_args && self.project_macros.contains(&token.text) {
                        calls.push((token.clone(), true));
                    }
                    if self.macros.contains_key(&token.text) {
                        nested.push((token.text.clone(), inner.to_vec()));
                    }
                }
            }
        }
        for (token, from_args) in calls {
            if !seen.insert((token.span.start_line, token.span.start_col, token.text.clone())) {
                continue;
            }
            let detail = (!from_args).then(|| format!("name:{}", token.text));
            self.occurrences.push(OccurrenceRef {
                role: "call".to_string(),
                line: token.span.start_line,
                col: token.span.start_col,
                enclosing_id: enclosing_id.to_string(),
                span: token.span,
                detail,
            });
        }
        for (inner_name, inner) in nested {
            self.expand_invocation(&inner_name, &inner, enclosing_id, depth + 1, seen);
        }
    }

    /// A `read` or `write` for every field access under the scope.
    ///
    /// A field in callee position (`obj.method()`) is skipped: `collect_calls`
    /// already recorded it, and one site must not yield two occurrences.
    fn collect_references(&mut self, scope: TsNode<'_>, enclosing_id: &str) {
        for node in owned_descendants(scope) {
            if node.kind() != "field_expression" {
                continue;
            }
            let is_callee = node.parent().is_some_and(|parent| {
                parent.kind() == "call_expression"
                    && parent.child_by_field_name("function") == Some(node)
            });
            if is_callee {
                continue;
            }
            // A field handed to a call is recorded with the call.
            let is_argument = node.parent().is_some_and(|parent| {
                parent.kind() == "arguments"
                    && parent.parent().is_some_and(|call| {
                        call.kind() == "call_expression"
                            && call.child_by_field_name("function").and_then(plain_name).is_some()
                    })
            });
            if is_argument {
                continue;
            }
            if let Some(field) = node.child_by_field_name("field") {
                let role = if is_assignment_target(node) { "write" } else { "read" };
                self.add_occurrence(role, field, enclosing_id);
            }
        }
    }

    /// An `annotation` for every type name under a type, once per site.
    ///
    /// `&dyn Labeled` gives `Labeled`, `Result<Matcher, Error>` gives all three
    /// names, `store::Engine` gives `Engine` as the annotation. The earlier segments
    /// of a path (`store`, `crate`, `super`) are `read` occurrences enclosed the same
    /// way (`record_path_prefixes`). A bare `Foo` is its own `type_identifier`. A site
    /// already recorded is skipped.
    fn record_annotation(&mut self, type_node: TsNode<'_>, enclosing_id: &str) {
        let mut idents = walk_type(type_node, "type_identifier");
        idents.sort_by_key(|n| n.start_byte());
        for name_node in idents {
            if self.recorded.insert(name_node.start_byte()) {
                self.add_occurrence("annotation", name_node, enclosing_id);
            }
        }
        self.record_path_prefixes(type_node, enclosing_id);
    }

    /// The type names a generic list, a bounds list or a `where` clause uses.
    ///
    /// The name of a type parameter is its declaration, not a use, so only the
    /// bounds and the default of a `type_parameter` are recorded, and the type of
    /// a `const_parameter`. Lifetimes are skipped.
    fn record_generics(&mut self, item: TsNode<'_>, enclosing_id: &str) {
        if let Some(params) = item.child_by_field_name("type_parameters") {
            for param in ts::children(params) {
                match param.kind() {
                    "type_parameter" => {
                        for field in ["bounds", "default_type"] {
                            if let Some(node) = param.child_by_field_name(field) {
                                self.record_annotation(node, enclosing_id);
                            }
                        }
                    }
                    "const_parameter" => {
                        if let Some(node) = param.child_by_field_name("type") {
                            self.record_annotation(node, enclosing_id);
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(bounds) = item.child_by_field_name("bounds") {
            self.record_annotation(bounds, enclosing_id);
        }
        for child in ts::children(item) {
            if child.kind() == "where_clause" {
                self.record_annotation(child, enclosing_id);
            }
        }
    }

    /// The types written inside a function body: `let x: T`, `e as T`, and a
    /// closure's parameter and return types. Nested `fn` items are not entered;
    /// they are nodes of their own. Closures belong to the enclosing function.
    fn collect_body_types(&mut self, body: TsNode<'_>, enclosing_id: &str) {
        for node in owned_descendants(body) {
            match node.kind() {
                "let_declaration" | "type_cast_expression" => {
                    if let Some(type_node) = node.child_by_field_name("type") {
                        self.record_annotation(type_node, enclosing_id);
                    }
                }
                "closure_expression" => {
                    if let Some(params) = node.child_by_field_name("parameters") {
                        for param in ts::children(params) {
                            if param.kind() == "parameter"
                                && let Some(type_node) = param.child_by_field_name("type")
                            {
                                self.record_annotation(type_node, enclosing_id);
                            }
                        }
                    }
                    if let Some(type_node) = node.child_by_field_name("return_type") {
                        self.record_annotation(type_node, enclosing_id);
                    }
                }
                _ => {}
            }
        }
    }

    /// The types of a tuple struct's (or tuple variant's) fields.
    fn record_tuple_field_types(&mut self, list: TsNode<'_>, enclosing_id: &str) {
        for child in ts::children(list) {
            if child.kind() == "visibility_modifier" {
                self.record_visibility_modifier(child, enclosing_id);
            }
        }
        let mut cursor = list.walk();
        let types: Vec<TsNode<'_>> = list.children_by_field_name("type", &mut cursor).collect();
        for type_node in types {
            self.record_annotation(type_node, enclosing_id);
        }
    }

    /// The uses inside a const, a static, an associated const or an enum discriminant:
    /// the type is an annotation and the `value` is scanned like a body, all enclosed
    /// by `id`. The order is `on_callable`'s, so `recorded` keeps one occurrence per site.
    fn scan_value(&mut self, item: TsNode<'_>, id: &str) {
        if let Some(type_node) = item.child_by_field_name("type") {
            self.record_annotation(type_node, id);
        }
        if let Some(value) = item.child_by_field_name("value") {
            self.collect_body_types(value, id);
            self.collect_calls(value, id);
            self.collect_references(value, id);
            self.record_path_prefixes(value, id);
            self.walk_values(value, false, id);
        }
    }

    /// PARAMETER nodes for a function's parameters, plus their annotations.
    ///
    /// `self` is skipped: it names the receiver, not a declared parameter, and
    /// carries no type of its own.
    fn collect_parameters(
        &mut self,
        function: TsNode<'_>,
        function_qname: &str,
        function_id: &str,
    ) -> PyResult<()> {
        let Some(parameters) = function.child_by_field_name("parameters") else {
            return Ok(());
        };
        for parameter in ts::children(parameters) {
            if parameter.kind() != "parameter" {
                continue;
            }
            let Some(pattern) = parameter.child_by_field_name("pattern") else {
                continue;
            };
            let name = self.text(pattern);
            if name.is_empty() {
                continue;
            }
            let type_node = parameter.child_by_field_name("type");
            let metadata = PyDict::new(self.py);
            metadata.set_item("annotation", type_node.map(|n| self.text(n)))?;
            metadata.set_item("name_span", ts::span(pattern))?;

            let qname = format!("{function_qname}.{name}");
            let id = node_id(self.project_name, &qname, NodeKind::Parameter.as_str());
            if self.graph.has_node(&id) {
                continue;
            }
            let node = Node {
                id: id.clone(),
                kind: NodeKind::Parameter,
                qualified_name: qname,
                name,
                file_path: Some(self.file_rel.to_string()),
                span: Some(ts::span(parameter)),
                metadata: metadata.unbind(),
            };
            self.graph.insert_node(id.clone(), Py::new(self.py, node)?);
            self.push_relation(function_id.to_string(), id.clone(), RelationKind::Declares)?;

            if let Some(type_node) = type_node {
                self.record_annotation(type_node, &id);
            }
        }
        Ok(())
    }

    /// ATTRIBUTE nodes for a struct's fields, plus their annotations.
    fn collect_fields(
        &mut self,
        type_node: TsNode<'_>,
        type_qname: &str,
        type_id: &str,
    ) -> PyResult<()> {
        let Some(body) = type_node.child_by_field_name("body") else {
            return Ok(());
        };
        if type_node.kind() == "enum_item" {
            return self.collect_variants(body, type_qname, type_id);
        }
        if body.kind() == "ordered_field_declaration_list" {
            self.record_tuple_field_types(body, type_id);
            return Ok(());
        }
        if body.kind() != "field_declaration_list" {
            return Ok(());
        }
        self.collect_named_fields(body, type_qname, type_id)
    }

    /// ATTRIBUTE nodes for the named fields of a struct, a union or a struct variant,
    /// plus their annotations.
    fn collect_named_fields(
        &mut self,
        body: TsNode<'_>,
        owner_qname: &str,
        owner_id: &str,
    ) -> PyResult<()> {
        for field in ts::children(body) {
            if field.kind() != "field_declaration" {
                continue;
            }
            let (Some(name_node), field_type) = (
                field.child_by_field_name("name"),
                field.child_by_field_name("type"),
            ) else {
                continue;
            };
            let name = self.text(name_node);
            let qname = format!("{owner_qname}.{name}");
            let id = self.declare(&qname, &name, NodeKind::Attribute, field, name_node)?;
            self.push_relation(owner_id.to_string(), id.clone(), RelationKind::Declares)?;
            if let Some(field_type) = field_type {
                self.record_annotation(field_type, &id);
            }
        }
        Ok(())
    }

    /// One CLASS `Enum.Variant` per variant (it is called like a tuple struct), declared
    /// by the enum. Payload types are enclosed by the variant, the named fields of a
    /// struct variant are ATTRIBUTEs of it, and an explicit discriminant is scanned.
    fn collect_variants(
        &mut self,
        list: TsNode<'_>,
        enum_qname: &str,
        enum_id: &str,
    ) -> PyResult<()> {
        for variant in ts::children(list) {
            if variant.kind() != "enum_variant" {
                continue;
            }
            let Some(name_node) = variant.child_by_field_name("name") else {
                continue;
            };
            let name = self.text(name_node);
            let qname = format!("{enum_qname}.{name}");
            let id = self.declare(&qname, &name, NodeKind::Class, variant, name_node)?;
            self.push_relation(enum_id.to_string(), id.clone(), RelationKind::Declares)?;
            if let Some(body) = variant.child_by_field_name("body") {
                match body.kind() {
                    "ordered_field_declaration_list" => self.record_tuple_field_types(body, &id),
                    "field_declaration_list" => self.collect_named_fields(body, &qname, &id)?,
                    _ => {}
                }
            }
            self.scan_value(variant, &id);
        }
        Ok(())
    }

    /// METHOD nodes for the methods a trait declares, with or without a default
    /// body: a call through a bound or a trait object lands on the trait's
    /// method, and without a node for it the call went nowhere. Its associated types
    /// are TYPE_ALIASes and its associated consts VARIABLEs, `Trait.Name`.
    fn collect_trait_methods(
        &mut self,
        trait_node: TsNode<'_>,
        trait_qname: &str,
        trait_id: &str,
    ) -> PyResult<()> {
        let Some(body) = trait_node.child_by_field_name("body") else {
            return Ok(());
        };
        for item in ts::children(body) {
            let kind = match item.kind() {
                "associated_type" => NodeKind::TypeAlias,
                "const_item" => NodeKind::Variable,
                "function_item" | "function_signature_item" => NodeKind::Method,
                _ => continue,
            };
            let Some(name_node) = item.child_by_field_name("name") else {
                continue;
            };
            let name = self.text(name_node);
            let qname = format!("{trait_qname}.{name}");
            let id = self.declare(&qname, &name, kind, item, name_node)?;
            self.push_relation(trait_id.to_string(), id.clone(), RelationKind::Declares)?;
            match item.kind() {
                "associated_type" => self.record_generics(item, &id),
                "const_item" => self.scan_value(item, &id),
                _ => self.on_callable(item, &qname, &id)?,
            }
        }
        Ok(())
    }

    fn on_function_item(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}::{name}", self.module_qname);
        let id = self.declare(&qname, &name, NodeKind::Function, node, name_node)?;
        self.on_callable(node, &qname, &id)
    }

    /// The parts shared by a free function and a method.
    fn on_callable(
        &mut self,
        node: TsNode<'_>,
        qname: &str,
        id: &str,
    ) -> PyResult<()> {
        self.collect_parameters(node, qname, id)?;
        self.record_generics(node, id);
        if let Some(return_type) = node.child_by_field_name("return_type") {
            self.record_annotation(return_type, id);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.collect_body_types(body, id);
        }
        self.collect_calls(node, id);
        self.collect_references(node, id);
        // Parameter and return types were claimed above; this sweeps the expression
        // paths (`Foo::new()`, `inner::f()`, patterns, struct literals, macros).
        self.record_path_prefixes(node, id);
        self.collect_values(node, id);
        self.collect_flow(node, qname, id)?;
        // Items declared inside this one (functions, structs, enums, consts, impls, ...)
        // are nodes of their own, qualified under the function: `module_qname` is the
        // function's qname while they are dispatched, and restored on every path.
        if let Some(body) = node.child_by_field_name("body") {
            let items = body_items(body);
            if !items.is_empty() {
                let outer = std::mem::replace(&mut self.module_qname, qname.to_string());
                let result = self.dispatch_items(items);
                self.module_qname = outer;
                result?;
            }
        }
        Ok(())
    }

    /// What the function's values hold, for the flow pass: locals that are called
    /// (`let f = g; f()`), the names the function returns, and function-valued
    /// fields of struct expressions (`S { f: g }`).
    fn collect_flow(&mut self, callable: TsNode<'_>, qname: &str, id: &str) -> PyResult<()> {
        let Some(body) = callable.child_by_field_name("body") else {
            return Ok(());
        };
        let nodes = owned_descendants(body);

        let mut called: HashSet<String> = HashSet::new();
        for node in &nodes {
            if node.kind() != "call_expression" {
                continue;
            }
            let Some(function) = node.child_by_field_name("function") else {
                continue;
            };
            let base = match function.kind() {
                "identifier" => Some(function),
                "index_expression" => ts::named_children(function).first().copied().filter(|o| o.kind() == "identifier"),
                _ => None,
            };
            if let Some(base) = base {
                called.insert(self.text(base));
            }
        }

        // A local that is only a container the called one is read from - looped
        // over, indexed - holds the functions too.
        let mut relevant = called.clone();
        for _ in 0..3 {
            for node in &nodes {
                let (left, right) = match node.kind() {
                    "let_declaration" => (node.child_by_field_name("pattern"), node.child_by_field_name("value")),
                    "assignment_expression" => (node.child_by_field_name("left"), node.child_by_field_name("right")),
                    "for_expression" => (node.child_by_field_name("pattern"), node.child_by_field_name("value")),
                    _ => continue,
                };
                let (Some(left), Some(right)) = (left, right) else {
                    continue;
                };
                if !relevant.contains(&self.text(left)) {
                    continue;
                }
                let source = match right.kind() {
                    "identifier" => Some(right),
                    "index_expression" | "reference_expression" | "unary_expression" => {
                        ts::named_children(right).first().copied().filter(|o| o.kind() == "identifier")
                    }
                    _ => None,
                };
                if let Some(source) = source {
                    relevant.insert(self.text(source));
                }
            }
        }
        let called = relevant;

        let mut locals: HashMap<String, String> = HashMap::new();
        for node in &nodes {
            if !matches!(node.kind(), "let_declaration" | "for_expression") {
                continue;
            }
            let Some(pattern) = node.child_by_field_name("pattern") else {
                continue;
            };
            if pattern.kind() != "identifier" {
                continue;
            }
            let name = self.text(pattern);
            if !called.contains(&name) || locals.contains_key(&name) {
                continue;
            }
            let local_qname = format!("{qname}.{name}");
            let local_id = node_id(self.project_name, &local_qname, NodeKind::Variable.as_str());
            if !self.graph.has_node(&local_id) {
                let metadata = PyDict::new(self.py);
                metadata.set_item("name_span", ts::span(pattern))?;
                let local = Node {
                    id: local_id.clone(),
                    kind: NodeKind::Variable,
                    qualified_name: local_qname,
                    name: name.clone(),
                    file_path: Some(self.file_rel.to_string()),
                    span: Some(ts::span(*node)),
                    metadata: metadata.unbind(),
                };
                self.graph.insert_node(local_id.clone(), Py::new(self.py, local)?);
                self.push_relation(id.to_string(), local_id.clone(), RelationKind::Declares)?;
            }
            locals.insert(name, local_id);
        }

        for node in &nodes {
            match node.kind() {
                "let_declaration" | "assignment_expression" | "for_expression" => {
                    let (left, right) = if matches!(node.kind(), "let_declaration" | "for_expression") {
                        (node.child_by_field_name("pattern"), node.child_by_field_name("value"))
                    } else {
                        (node.child_by_field_name("left"), node.child_by_field_name("right"))
                    };
                    let (Some(left), Some(right)) = (left, right) else {
                        continue;
                    };
                    if let Some(var_id) = locals.get(&self.text(left)).cloned() {
                        self.record_aliases(right, &var_id, 0);
                    }
                }
                "return_expression" => {
                    if let Some(value) = ts::named_children(*node).first().copied()
                        && let Some(name) = plain_name(value)
                    {
                        self.add_occurrence_with("alias", name, id, Some("return".into()));
                    }
                }
                "struct_expression" => self.struct_expression_fields(*node),
                _ => {}
            }
        }
        // The function's tail expression is returned as surely as `return` does.
        if let Some(tail) = ts::named_children(body).last().copied()
            && let Some(name) = plain_name(tail)
        {
            self.add_occurrence_with("alias", name, id, Some("return".into()));
        }
        Ok(())
    }

    /// What `value` hands to the variable it is bound to.
    fn record_aliases(&mut self, value: TsNode<'_>, variable_id: &str, depth: usize) {
        if let Some(name) = plain_name(value) {
            self.add_occurrence_with("alias", name, variable_id, None);
            return;
        }
        match value.kind() {
            "call_expression" => {
                if let Some(name) = callee_name(value) {
                    self.add_occurrence_with("alias", name, variable_id, Some("result".into()));
                }
            }
            "reference_expression" | "unary_expression" | "type_cast_expression" if depth < 3 => {
                if let Some(inner) = value.child_by_field_name("value").or_else(|| ts::named_children(value).last().copied()) {
                    self.record_aliases(inner, variable_id, depth + 1);
                }
            }
            "array_expression" | "tuple_expression" if depth < 3 => {
                for element in ts::named_children(value) {
                    self.record_aliases(element, variable_id, depth + 1);
                }
            }
            "index_expression" => {
                if let Some(base) = ts::named_children(value).first().copied().and_then(plain_name) {
                    self.add_occurrence_with("alias", base, variable_id, None);
                }
            }
            _ => {}
        }
    }

    /// `S { f: g }`: the field `S.f` holds what `g` resolves to.
    fn struct_expression_fields(&mut self, literal: TsNode<'_>) {
        let Some(name) = literal.child_by_field_name("name") else {
            return;
        };
        let type_name = self.text(name);
        let Some(body) = literal.child_by_field_name("body") else {
            return;
        };
        for initializer in ts::named_children(body) {
            if initializer.kind() != "field_initializer" {
                continue;
            }
            let (Some(field), Some(value)) = (
                initializer.child_by_field_name("field"),
                initializer.child_by_field_name("value"),
            ) else {
                continue;
            };
            let field_qname = format!("{}::{type_name}.{}", self.module_qname, self.text(field));
            let field_id = node_id(self.project_name, &field_qname, NodeKind::Attribute.as_str());
            self.record_aliases(value, &field_id, 0);
        }
    }

    fn on_impl_item(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let type_name = self.impl_type_name(node);
        // The trait first: its leading name is the `base`, already marked as seen
        // when the rest of the header is recorded.
        self.collect_impl_trait(node, &type_name)?;
        let file_id = self.file_id;
        if let Some(type_node) = node.child_by_field_name("type") {
            self.record_annotation(type_node, file_id);
        }
        self.record_generics(node, file_id);
        let Some(body) = node.child_by_field_name("body") else {
            return Ok(());
        };
        let prefix = if type_name.is_empty() {
            String::new()
        } else {
            format!("{type_name}.")
        };
        for item in ts::children(body) {
            let kind = match item.kind() {
                "type_item" => NodeKind::TypeAlias,
                "const_item" => NodeKind::Variable,
                "function_item" => NodeKind::Method,
                _ => continue,
            };
            let Some(name_node) = item.child_by_field_name("name") else {
                continue;
            };
            let name = self.text(name_node);
            let qname = format!("{}::{prefix}{name}", self.module_qname);
            let id = self.declare(&qname, &name, kind, item, name_node)?;
            match item.kind() {
                "type_item" => {
                    self.record_generics(item, &id);
                    if let Some(type_node) = item.child_by_field_name("type") {
                        self.record_annotation(type_node, &id);
                    }
                }
                "const_item" => self.scan_value(item, &id),
                _ => self.on_callable(item, &qname, &id)?,
            }
        }
        Ok(())
    }

    /// `impl Trait for Type` as a `base` occurrence.
    ///
    /// The base is enclosed by the type's CLASS when the project declares it, else by the FILE.
    ///
    /// Rust declares this relationship on a separate `impl` rather than on the
    /// type itself (unlike Python's and Go's base lists), so the implementing
    /// type's id has to be recomputed here instead of reused from
    /// `declare_named`.
    fn collect_impl_trait(&mut self, impl_node: TsNode<'_>, type_name: &str) -> PyResult<()> {
        let Some(trait_node) = impl_node.child_by_field_name("trait") else {
            return Ok(());
        };
        let idents = walk_type(trait_node, "type_identifier");
        let trait_name_node = leftmost(&idents).unwrap_or(trait_node);

        // The implementing type's class when the project declares it; otherwise
        // the file, so `impl Trait for External` keeps its `base`.
        let mut enclosing = self.file_id.to_string();
        if !type_name.is_empty() {
            let type_qname = format!("{}::{type_name}", self.module_qname);
            let type_id = node_id(self.project_name, &type_qname, NodeKind::Class.as_str());
            if self.graph.has_node(&type_id) {
                enclosing = type_id;
            }
        }
        self.recorded.insert(trait_name_node.start_byte());
        self.add_occurrence("base", trait_name_node, &enclosing);
        // The trait's generic arguments (`From<Matcher>`) are uses of types.
        let file_id = self.file_id;
        self.record_annotation(trait_node, file_id);
        Ok(())
    }

    fn impl_type_name(&self, impl_node: TsNode<'_>) -> String {
        let Some(type_node) = impl_node.child_by_field_name("type") else {
            return String::new();
        };
        let idents = walk_type(type_node, "type_identifier");
        match leftmost(&idents) {
            Some(node) => self.text(node),
            None => self.text(type_node),
        }
    }

    fn on_use_declaration(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let file_id = self.file_id;
        self.record_visibility(node, file_id);
        let import_path = self.text_or_empty(node.child_by_field_name("argument"));
        if import_path.is_empty() {
            return Ok(());
        }
        let origin = super::deps::classify_import(&import_path, self.crate_name, self.deps);
        // The leaves are recorded BEFORE `declare_import`, which returns early when an
        // identical `use` text already made the IMPORT node: each site still has its own.
        let import_qname = format!("{}::{import_path}", self.file_rel);
        let import_id = node_id(self.project_name, &import_qname, NodeKind::Import.as_str());
        if let Some(argument) = node.child_by_field_name("argument") {
            self.collect_use_leaves(argument, "", &import_id);
        }
        self.declare_import(node, &import_path, origin)
    }

    /// One site per name a use tree imports. `*`, `self`, `crate`, `super` and the `as`
    /// alias are not sites: for `use a::b as c` the leaf is `b`.
    fn collect_use_leaves(&mut self, node: TsNode<'_>, prefix: &str, import_id: &str) {
        match node.kind() {
            "identifier" => {
                let path = join_use_path(prefix, &self.text(node));
                self.record_use_leaf(node, path, import_id);
            }
            "scoped_identifier" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return;
                };
                if name.kind() != "identifier" {
                    return;
                }
                let path = join_use_path(prefix, &compact(&self.text(node)));
                self.record_use_leaf(name, path, import_id);
            }
            "use_as_clause" => {
                if let Some(original) = node.child_by_field_name("path") {
                    self.collect_use_leaves(original, prefix, import_id);
                }
            }
            "scoped_use_list" => {
                let inner = match node.child_by_field_name("path") {
                    Some(path) => join_use_path(prefix, &compact(&self.text(path))),
                    None => prefix.to_string(),
                };
                if let Some(list) = node.child_by_field_name("list") {
                    self.collect_use_leaves(list, &inner, import_id);
                }
            }
            "use_list" => {
                for child in ts::children(node) {
                    if child.is_named() {
                        self.collect_use_leaves(child, prefix, import_id);
                    }
                }
            }
            _ => {}
        }
    }

    fn record_use_leaf(&mut self, name_node: TsNode<'_>, path: String, import_id: &str) {
        let origin = super::deps::classify_import(&path, self.crate_name, self.deps);
        let span = ts::span(name_node);
        self.use_sites.push(UseSite {
            occurrence: OccurrenceRef {
                role: "import".to_string(),
                line: span.start_line,
                col: span.start_col,
                enclosing_id: import_id.to_string(),
                span,
                detail: None,
            },
            path,
            module_qname: self.module_qname.clone(),
            origin,
            mod_decl: false,
        });
    }

    fn declare_import(
        &mut self,
        node: TsNode<'_>,
        import_path: &str,
        origin: &str,
    ) -> PyResult<()> {
        let import_qname = format!("{}::{import_path}", self.file_rel);
        let import_id = node_id(self.project_name, &import_qname, NodeKind::Import.as_str());
        if self.graph.has_node(&import_id) {
            return Ok(());
        }

        let metadata = PyDict::new(self.py);
        metadata.set_item("origin", origin)?;
        metadata.set_item("import_path", import_path)?;
        let import_node = Node {
            id: import_id.clone(),
            kind: NodeKind::Import,
            qualified_name: import_qname,
            name: import_path.to_string(),
            file_path: Some(self.file_rel.to_string()),
            span: Some(ts::span(node)),
            metadata: metadata.unbind(),
        };
        self.graph.insert_node(import_id.clone(), Py::new(self.py, import_node)?);
        self.push_relation(
            self.file_id.to_string(),
            import_id.clone(),
            RelationKind::Declares,
        )?;

        if origin == "internal" {
            // Deferred: bound to the real MODULE node once every module
            // exists (falling back to an EXTERNAL_SYMBOL otherwise). The
            // IMPORTS edge is deferred with it, since it needs the same
            // target.
            self.internal_imports.push((
                import_id,
                import_path.to_string(),
                self.module_qname.clone(),
                self.file_id.to_string(),
            ));
            return Ok(());
        }

        let sym_id = node_id(self.project_name, import_path, NodeKind::ExternalSymbol.as_str());
        if !self.graph.has_node(&sym_id) {
            let metadata = PyDict::new(self.py);
            metadata.set_item("origin", origin)?;
            let symbol = Node {
                id: sym_id.clone(),
                kind: NodeKind::ExternalSymbol,
                qualified_name: import_path.to_string(),
                name: import_path.rsplit("::").next().unwrap_or(import_path).to_string(),
                file_path: None,
                span: None,
                metadata: metadata.unbind(),
            };
            self.graph.insert_node(sym_id.clone(), Py::new(self.py, symbol)?);
        }
        self.push_relation(self.file_id.to_string(), sym_id.clone(), RelationKind::Imports)?;
        self.push_relation(import_id, sym_id, RelationKind::ResolvesTo)
    }

    fn push_relation(
        &mut self,
        source_id: String,
        target_id: String,
        kind: RelationKind,
    ) -> PyResult<()> {
        let relation = Relation {
            source_id,
            target_id,
            kind,
            metadata: PyDict::new(self.py).unbind(),
        };
        self.graph.push_relation(Py::new(self.py, relation)?);
        Ok(())
    }
}

/// Methods and functions of the standard library that call a function they are
/// given, by name. The list is closed on purpose: a function handed to anything
/// else is a reference, since whether the callee calls it cannot be known from the
/// program. It applies only where the callee is not a function of the project.
fn calls_its_arguments(name: &str) -> bool {
    matches!(
        name,
        // iterators
        "map" | "for_each" | "filter" | "filter_map" | "flat_map" | "fold" | "try_fold" | "reduce"
            | "any" | "all" | "find" | "find_map" | "position" | "rposition" | "take_while"
            | "skip_while" | "map_while" | "inspect" | "scan" | "max_by" | "min_by" | "max_by_key"
            | "min_by_key" | "partition" | "from_fn" | "repeat_with" | "successors"
            // slices and vectors
            | "sort_by" | "sort_by_key" | "sort_unstable_by" | "sort_unstable_by_key"
            | "sort_by_cached_key" | "retain" | "dedup_by_key" | "binary_search_by"
            | "binary_search_by_key"
            // options, results and maps
            | "and_then" | "or_else" | "map_err" | "unwrap_or_else" | "map_or" | "map_or_else"
            | "ok_or_else" | "then" | "is_some_and" | "is_ok_and" | "get_or_insert_with"
            | "or_insert_with" | "and_modify" | "then_with"
            // threads and unwinding
            | "spawn" | "catch_unwind" | "scope" | "call_once"
    )
}
