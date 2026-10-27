//! Structural extraction from Go: packages, types, functions, imports.
//!
//! Simpler than Python and TypeScript: Go has no nested declaration scopes,
//! so there are no stacks — only a file's top-level declarations are walked,
//! and the value of a package-level `var` or `const` is scanned with the
//! variable as the enclosing node.

use std::collections::{HashMap, HashSet};

use crate::py::prelude::*;
use crate::py::types::PyDict;
use tree_sitter::Node as TsNode;

use crate::graph::Graph;
use crate::ids::node_id;
use crate::node::{Node, NodeKind};
use crate::occurrence::OccurrenceRef;
use crate::relation::{Relation, RelationKind};
use crate::span::Span;
use crate::ts;

/// A node's descendants in the order the Python version walks them: a stack
/// popped from the end, i.e. depth-first and right to left. Unlike `walk_type`,
/// which reads in source order, this keeps its order.
fn descendants<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        out.push(current);
        stack.extend(ts::children(current));
    }
    out
}

/// A type with pointers and parentheses removed: `*T` and `(T)` give `T`.
fn strip_type<'tree>(mut ty: TsNode<'tree>) -> TsNode<'tree> {
    while matches!(ty.kind(), "pointer_type" | "parenthesized_type") {
        match ts::named_children(ty).first().copied() {
            Some(inner) => ty = inner,
            None => break,
        }
    }
    ty
}

/// The syntactic type of a `literal_value`. For `T{..}` it is the composite
/// literal's type; for an elided inner literal (`[]T{{..}}`, `map[K]V{k: {..}}`)
/// it is the element, key or value type of the enclosing literal. `None` when
/// the syntax does not say (a named type such as `type Ts []T`).
fn literal_type<'tree>(literal: TsNode<'tree>) -> Option<TsNode<'tree>> {
    let parent = literal.parent()?;
    match parent.kind() {
        "composite_literal" => parent.child_by_field_name("type").map(strip_type),
        "literal_element" => {
            let holder = parent.parent()?;
            let (outer, is_key) = if holder.kind() == "keyed_element" {
                let is_key = ts::named_children(holder).first().copied() == Some(parent);
                (holder.parent()?, is_key)
            } else {
                (holder, false)
            };
            let outer_ty = literal_type(outer)?;
            match outer_ty.kind() {
                "slice_type" | "array_type" | "implicit_length_array_type" => {
                    outer_ty.child_by_field_name("element").map(strip_type)
                }
                "map_type" => outer_ty
                    .child_by_field_name(if is_key { "key" } else { "value" })
                    .map(strip_type),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The node itself and every descendant of the given kind, depth-first and
/// left to right (source order). A match is not descended into. The root is
/// included, so a type that is itself a `type_identifier` is found.
fn walk_type<'tree>(node: TsNode<'tree>, kind: &str) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == kind {
            out.push(current);
        } else {
            let mut kids = ts::children(current);
            kids.reverse();
            stack.extend(kids);
        }
    }
    out
}

/// A predeclared Go type or constraint. None of them is a node of the graph:
/// the resolver answers with a position in builtin.go.
fn is_predeclared_type(name: &str) -> bool {
    matches!(
        name,
        "bool" | "string" | "error" | "any" | "comparable" | "int" | "int8" | "int16"
            | "int32" | "int64" | "uint" | "uint8" | "uint16" | "uint32" | "uint64"
            | "uintptr" | "float32" | "float64" | "complex64" | "complex128" | "byte"
            | "rune"
    )
}

/// The type parameters in scope at `node`: those declared by the enclosing
/// function, method, type spec or type alias (and, for a method, the names the
/// receiver binds, `T` in `func (b *Box[T]) Put(v T)`).
fn type_parameter_names(src: &[u8], node: TsNode<'_>) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut current = node.parent();
    while let Some(n) = current {
        if matches!(
            n.kind(),
            "function_declaration" | "method_declaration" | "type_spec" | "type_alias"
        ) {
            if let Some(params) = n.child_by_field_name("type_parameters") {
                for decl in walk_type(params, "type_parameter_declaration") {
                    for child in ts::children(decl) {
                        if child.kind() == "identifier"
                            && let Ok(text) = child.utf8_text(src)
                        {
                            names.insert(text.to_string());
                        }
                    }
                }
            }
            if n.kind() == "method_declaration"
                && let Some(receiver) = n.child_by_field_name("receiver")
            {
                for args in walk_type(receiver, "type_arguments") {
                    for id in walk_type(args, "type_identifier") {
                        if let Ok(text) = id.utf8_text(src) {
                            names.insert(text.to_string());
                        }
                    }
                }
            }
        }
        current = n.parent();
    }
    names
}

/// The callee's name token; None when an expression result is called.
fn called_name<'tree>(function: TsNode<'tree>) -> Option<TsNode<'tree>> {
    match function.kind() {
        // foo()
        "identifier" => Some(function),
        // pkg.Foo() / recv.Method()
        "selector_expression" => function.child_by_field_name("field"),
        _ => None,
    }
}

/// The name a chain of selectors ends in - the position the resolver is asked
/// about - when `node` is a name, a selector or a parenthesised one.
fn plain_name<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    match node.kind() {
        "identifier" => Some(node),
        "selector_expression" => node.child_by_field_name("field"),
        "parenthesized_expression" => ts::named_children(node).first().copied().and_then(plain_name),
        _ => None,
    }
}

/// The name a call's callee ends in.
fn callee_name<'tree>(call: TsNode<'tree>) -> Option<TsNode<'tree>> {
    call.child_by_field_name("function").and_then(plain_name)
}

/// Whether the node is the left-hand side of an assignment or increment.
fn is_assignment_target(node: TsNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if matches!(parent.kind(), "inc_statement" | "dec_statement") {
        return true;
    }
    if parent.kind() == "expression_list" {
        return parent.parent().is_some_and(|grandparent| {
            let is_left = grandparent.child_by_field_name("left") == Some(parent);
            // `for k, v = range m` assigns; `for k, v := range m` declares.
            (grandparent.kind() == "assignment_statement" && is_left)
                || (grandparent.kind() == "range_clause"
                    && is_left
                    && !has_token(grandparent, ":="))
        });
    }
    false
}

/// Whether the node has an anonymous token child with this text (`:=`, `=`).
fn has_token(node: TsNode<'_>, token: &str) -> bool {
    ts::children(node)
        .into_iter()
        .any(|child| !child.is_named() && child.kind() == token)
}

/// Whether an identifier is a name being declared (a function, var, const,
/// parameter or type-parameter name, the left side of `:=`, a type switch alias)
/// rather than a use of something declared elsewhere.
fn is_declaration_name(node: TsNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if matches!(
        parent.kind(),
        "function_declaration"
            | "var_spec"
            | "const_spec"
            | "parameter_declaration"
            | "variadic_parameter_declaration"
            | "type_parameter_declaration"
    ) {
        let mut cursor = parent.walk();
        let named = parent
            .children_by_field_name("name", &mut cursor)
            .any(|name| name == node);
        return named;
    }
    let (holder, side) = if parent.kind() == "expression_list" {
        match parent.parent() {
            Some(holder) => (holder, parent),
            None => return false,
        }
    } else {
        (parent, node)
    };
    match holder.kind() {
        "short_var_declaration" => holder.child_by_field_name("left") == Some(side),
        "range_clause" | "receive_statement" => {
            holder.child_by_field_name("left") == Some(side) && has_token(holder, ":=")
        }
        "type_switch_statement" => holder.child_by_field_name("alias") == Some(side),
        _ => false,
    }
}

/// The `type_identifier` naming a (possibly qualified) type.
fn type_name_node<'tree>(type_node: Option<TsNode<'tree>>) -> Option<TsNode<'tree>> {
    let type_node = type_node?;
    match type_node.kind() {
        // Animal
        "type_identifier" => Some(type_node),
        // pkg.Base → Base
        "qualified_type" => type_node.child_by_field_name("name"),
        // e.g. instantiating the generic Base[int]
        _ => None,
    }
}

/// The identifier go/types names an embedded field after: `T`, `*T`, `pkg.T`,
/// `T[A]` and `pkg.T[A]` all give the last identifier of the type's name.
fn embedded_name_node<'tree>(type_node: Option<TsNode<'tree>>) -> Option<TsNode<'tree>> {
    let type_node = type_node?;
    if type_node.kind() == "generic_type" {
        return type_name_node(type_node.child_by_field_name("type"));
    }
    type_name_node(Some(type_node))
}

/// The name an import brings into a file, and the position go/types gives it:
/// the alias identifier when there is one, else the start of the path literal.
/// A package qualifier binds to the import only when the resolver answers with
/// exactly this position, which is what keeps a local that shadows a package out.
pub struct ImportSite {
    pub line: u32,
    pub col: u32,
    pub path: String,
    pub origin: &'static str,
}

/// The package name an import path is assumed to declare, as gopls assumes it
/// (`ImportPathToAssumedName`): the last element, or the one before it when the
/// last is a major-version suffix `vN`; a `go-` prefix is dropped and the name
/// is cut at the first character that cannot be part of an identifier.
pub fn assumed_package_name(path: &str) -> String {
    let mut parts: Vec<&str> = path.split('/').collect();
    let mut base = parts.pop().unwrap_or("");
    let is_version = base.len() > 1
        && base.starts_with('v')
        && base[1..].bytes().all(|b| b.is_ascii_digit());
    if is_version && let Some(previous) = parts.last() {
        base = previous;
    }
    let base = base.strip_prefix("go-").unwrap_or(base);
    base.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect()
}

pub struct GoExtractor<'a> {
    py: Python<'a>,
    graph: &'a mut Graph,
    source: &'a [u8],
    project_name: &'a str,
    package_qname: &'a str,
    file_id: &'a str,
    file_rel: &'a str,
    /// The project's module and its dependencies — for classifying imports.
    module_path: Option<&'a str>,
    required: &'a HashSet<String>,
    occurrences: Vec<OccurrenceRef>,
    /// `(import node id, path, importing file, span of the path literal)` for
    /// internal imports: the adapter substitutes the real MODULE node once every
    /// package exists, and emits IMPORTS from the file at the same time.
    internal_imports: Vec<(String, String, String, Span)>,
    /// The local names the file's imports bind (an alias, else the assumed name
    /// of the path). Dot and blank imports are not in it.
    package_names: HashSet<String>,
    import_sites: Vec<ImportSite>,
    /// Start positions that already carry a `call`, `read` or `write` occurrence
    /// (a callee, a direct argument, a package qualifier, a literal key, a selector
    /// field). A bare identifier at one of them is not recorded a second time.
    sites: HashSet<(u32, u32)>,
}

impl<'a> GoExtractor<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        py: Python<'a>,
        graph: &'a mut Graph,
        source: &'a [u8],
        project_name: &'a str,
        package_qname: &'a str,
        file_id: &'a str,
        file_rel: &'a str,
        module_path: Option<&'a str>,
        required: &'a HashSet<String>,
    ) -> Self {
        Self {
            py,
            graph,
            source,
            project_name,
            package_qname,
            file_id,
            file_rel,
            module_path,
            required,
            occurrences: Vec::new(),
            internal_imports: Vec::new(),
            package_names: HashSet::new(),
            import_sites: Vec::new(),
            sites: HashSet::new(),
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        self,
    ) -> (Vec<OccurrenceRef>, Vec<(String, String, String, Span)>, Vec<ImportSite>) {
        (self.occurrences, self.internal_imports, self.import_sites)
    }

    fn text(&self, node: TsNode<'_>) -> String {
        ts::text(node, self.source).into_owned()
    }

    /// Parses the file's top-level declarations.
    pub fn extract(&mut self, root: TsNode<'_>) -> PyResult<()> {
        self.collect_import_sites(root);
        for child in ts::children(root) {
            match child.kind() {
                "function_declaration" => self.on_function(child)?,
                "method_declaration" => self.on_method(child)?,
                "type_declaration" => self.on_type_declaration(child)?,
                "var_declaration" => self.on_value_specs(child, "var_spec")?,
                "const_declaration" => self.on_value_specs(child, "const_spec")?,
                "import_declaration" => self.on_import_declaration(child)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn declare(
        &mut self,
        qname: &str,
        name: &str,
        kind: NodeKind,
        full_node: TsNode<'_>,
        name_node: TsNode<'_>,
    ) -> PyResult<String> {
        let id = node_id(self.project_name, qname, kind.as_str());
        if self.graph.has_node(&id) {
            return Ok(id);
        }
        let metadata = PyDict::new(self.py);
        metadata.set_item("name_span", ts::span(name_node))?;
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
        if matches!(role, "call" | "read" | "write") {
            self.sites.insert((span.start_line, span.start_col));
        }
        self.occurrences.push(OccurrenceRef {
            role: role.to_string(),
            line: span.start_line,
            col: span.start_col,
            enclosing_id: enclosing_id.to_string(),
            span,
            detail,
        });
    }

    /// Reads every import spec of the file without touching the graph (so node
    /// order is unchanged): the names the imports bind, and the position the
    /// type checker gives each of them.
    fn collect_import_sites(&mut self, root: TsNode<'_>) {
        for declaration in ts::children(root) {
            if declaration.kind() != "import_declaration" {
                continue;
            }
            for spec in walk_type(declaration, "import_spec") {
                let Some(path_node) = spec.child_by_field_name("path") else {
                    continue;
                };
                let import_path = self.text(path_node).trim_matches('"').to_string();
                if import_path.is_empty() {
                    continue;
                }
                let name_node = spec.child_by_field_name("name");
                let local = match name_node {
                    Some(name) => self.text(name),
                    None => assumed_package_name(&import_path),
                };
                // `.` and `_` imports bind no qualifier.
                if local.is_empty() || local == "." || local == "_" {
                    continue;
                }
                let at = ts::span(name_node.unwrap_or(path_node));
                let origin = classify_import(&import_path, self.module_path, self.required);
                self.package_names.insert(local);
                self.import_sites.push(ImportSite {
                    line: at.start_line,
                    col: at.start_col,
                    path: import_path,
                    origin,
                });
            }
        }
    }

    /// The package name in `pkg.X` / `pkg.T`: a `read` whose detail is `package`.
    /// The resolution pass binds it to the package only when the type checker
    /// answers with the position of one of the file's imports.
    fn record_qualifier(&mut self, node: TsNode<'_>, enclosing_id: &str) {
        self.add_occurrence_with("read", node, enclosing_id, Some("package".to_string()));
    }

    /// Qualifiers of every `pkg.T` in a type. A `package_identifier` in a type
    /// position is always a package, so it is not filtered by the import names.
    fn record_type_qualifiers(&mut self, scope: TsNode<'_>, enclosing_id: &str) {
        for qualified in walk_type(scope, "qualified_type") {
            if let Some(package) = qualified.child_by_field_name("package") {
                self.record_qualifier(package, enclosing_id);
            }
        }
    }

    /// A call site for every `call_expression` inside the scope.
    fn collect_calls(&mut self, scope: TsNode<'_>, enclosing_id: &str) {
        for node in descendants(scope) {
            if node.kind() != "call_expression" {
                continue;
            }
            let Some(function) = node.child_by_field_name("function") else {
                continue;
            };
            let mut site = None;
            if let Some(name_node) = called_name(function) {
                self.add_occurrence("call", name_node, enclosing_id);
                site = Some(ts::span(name_node));
            } else if function.kind() == "call_expression" {
                // `f()()`: a call of whatever `f` returns. The inner call has its
                // own occurrence.
                if let Some(inner) = callee_name(function) {
                    self.add_occurrence_with("call", inner, enclosing_id, Some("result".into()));
                }
            } else if function.kind() == "index_expression" {
                // `table[key]()`: a call of whatever the container holds.
                if let Some(base) = function.child_by_field_name("operand").and_then(plain_name) {
                    self.add_occurrence("call", base, enclosing_id);
                }
            }
            // The names handed to the call say which parameter they are handed to.
            if let (Some(site), Some(arguments)) = (site, node.child_by_field_name("arguments")) {
                for (index, argument) in ts::named_children(arguments).into_iter().enumerate() {
                    if let Some(name) = plain_name(argument) {
                        let detail = format!("arg:{}:{}:{index}", site.start_line, site.start_col);
                        self.add_occurrence_with("read", name, enclosing_id, Some(detail));
                        // `sort.Slice(x, less)`: the library calls what it is given.
                        if calls_its_arguments(&self.text(function)) {
                            let detail = format!("c:{}:{}", site.start_line, site.start_col);
                            self.add_occurrence_with("callarg", name, enclosing_id, Some(detail));
                        }
                    }
                }
            }
        }
    }

    /// A `read`/`write` for every package or field selector.
    ///
    /// Selectors already recorded as a `call` (that is, `pkg.Foo` in
    /// `pkg.Foo()`) are skipped so one site does not yield two occurrences. A
    /// selector on an assignment's left-hand side yields `write`.
    ///
    /// This is also the only place a package qualifier is found in an expression:
    /// when the operand of any selector (callee and argument included) is an
    /// identifier naming one of the file's imports, the operand gets a `read`
    /// with detail `package`. A local that shadows the package passes this test
    /// and is rejected later, because the resolver answers with the local.
    ///
    /// Every other bare identifier is a `read`/`write` with detail `name:<ident>`
    /// (see `record_identifier`); the adapter drops the ones that are not a
    /// package-level name.
    fn collect_references(&mut self, scope: TsNode<'_>, enclosing_id: &str) {
        for node in descendants(scope) {
            if node.kind() == "keyed_element" {
                self.record_literal_key(node, enclosing_id);
                continue;
            }
            if node.kind() == "identifier" {
                // Runs after collect_calls and after the enclosing selector or keyed
                // element (descendants lists parents first), so `sites` is complete.
                self.record_identifier(node, enclosing_id);
                continue;
            }
            if node.kind() != "selector_expression" {
                continue;
            }
            if let Some(operand) = node.child_by_field_name("operand")
                && operand.kind() == "identifier"
            {
                let is_package = self.package_names.contains(&self.text(operand));
                if is_package {
                    self.record_qualifier(operand, enclosing_id);
                }
            }
            let is_callee = node.parent().is_some_and(|parent| {
                parent.kind() == "call_expression"
                    && parent.child_by_field_name("function") == Some(node)
            });
            if is_callee {
                continue;
            }
            // A selector handed to a call is recorded with the call.
            let is_argument = node.parent().is_some_and(|parent| {
                parent.kind() == "argument_list"
                    && parent.parent().is_some_and(|call| {
                        call.kind() == "call_expression"
                            && call.child_by_field_name("function").and_then(called_name).is_some()
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

    /// A bare identifier in expression position: a `read`, or a `write` on the
    /// left of an assignment, with detail `name:<ident>`. Nothing here knows what
    /// the name denotes; the adapter keeps the occurrence only when it is a
    /// package-level VARIABLE or FUNCTION of the file's package and go/types binds
    /// it there. Declaration names, `_` and sites that already have an occurrence
    /// are skipped.
    fn record_identifier(&mut self, node: TsNode<'_>, enclosing_id: &str) {
        let name = self.text(node);
        if name == "_" || is_declaration_name(node) {
            return;
        }
        let span = ts::span(node);
        if self.sites.contains(&(span.start_line, span.start_col)) {
            return;
        }
        let role = if is_assignment_target(node) { "write" } else { "read" };
        self.add_occurrence_with(role, node, enclosing_id, Some(format!("name:{name}")));
    }

    /// The key of a keyed struct literal (`T{Name: v}`, `&T{..}`, the elided
    /// `[]T{{Name: v}}`) names a field of T, so it is a `write` of that field by
    /// the enclosing definition, with detail `key`.
    ///
    /// Only an identifier key counts. Keys of a map, slice or array literal are
    /// values or indices, not fields, and are skipped when the literal's type is
    /// syntactically one of those (see `literal_type`). A type that cannot be read
    /// off the syntax (a named map type) is emitted; the adapter drops the keys
    /// that do not bind to a field. The predeclared `true`, `false`, `nil` and
    /// `iota` are never keys here (a field with one of those names is ignored).
    fn record_literal_key(&mut self, element: TsNode<'_>, enclosing_id: &str) {
        let Some(key) = ts::named_children(element).first().copied() else {
            return;
        };
        let key = if key.kind() == "literal_element" {
            match ts::named_children(key).first().copied() {
                Some(inner) => inner,
                None => return,
            }
        } else {
            key
        };
        if key.kind() != "identifier" {
            return;
        }
        if matches!(self.text(key).as_str(), "true" | "false" | "nil" | "iota") {
            return;
        }
        let Some(literal) = element.parent() else {
            return;
        };
        if let Some(literal_ty) = literal_type(literal)
            && matches!(
                literal_ty.kind(),
                "map_type" | "slice_type" | "array_type" | "implicit_length_array_type"
            )
        {
            return;
        }
        self.add_occurrence_with("write", key, enclosing_id, Some("key".to_string()));
    }

    /// A `base` for every embedded type.
    fn collect_bases(&mut self, type_node: TsNode<'_>, enclosing_id: &str) {
        if type_node.kind() == "struct_type" {
            for fdl in ts::children(type_node) {
                if fdl.kind() != "field_declaration_list" {
                    continue;
                }
                for fd in ts::children(fdl) {
                    if fd.kind() != "field_declaration" {
                        continue;
                    }
                    // A named field is not an embedded type.
                    if fd.child_by_field_name("name").is_some() {
                        continue;
                    }
                    if let Some(name_node) = embedded_name_node(fd.child_by_field_name("type")) {
                        self.add_occurrence("base", name_node, enclosing_id);
                    }
                    if let Some(embedded) = fd.child_by_field_name("type") {
                        self.record_type_qualifiers(embedded, enclosing_id);
                    }
                }
            }
            return;
        }

        // interface_type
        for elem in ts::children(type_node) {
            if elem.kind() != "type_elem" {
                continue;
            }
            // Several terms in one element form a type-set union (`A | B`,
            // `~int`), i.e. a generic constraint rather than embedding. Real
            // embedding has exactly one term.
            let terms = ts::named_children(elem);
            if terms.len() != 1 {
                continue;
            }
            if let Some(name_node) = type_name_node(Some(terms[0])) {
                self.add_occurrence("base", name_node, enclosing_id);
            }
            self.record_type_qualifiers(terms[0], enclosing_id);
        }
    }

    /// An `annotation` for every named type a type mentions.
    ///
    /// `*S`, `[]S`, `map[K]V`, `func(S) R`, `pkg.S` and `G[S]` each depend on
    /// all the names inside them, so each `type_identifier` (the name of a
    /// `qualified_type`, a generic's base and its arguments included) gets one.
    /// Predeclared names (`string`, `error`, `any`...) and type parameters in
    /// scope are skipped: neither is a node, one resolves into builtin.go and
    /// the other to a position with nothing behind it. A project that declares
    /// its own `error` or `any` loses that annotation; that is accepted.
    fn record_annotation(&mut self, type_node: TsNode<'_>, enclosing_id: &str) {
        let params = type_parameter_names(self.source, type_node);
        self.annotate_names(walk_type(type_node, "type_identifier"), enclosing_id, &params);
        self.record_type_qualifiers(type_node, enclosing_id);
    }

    /// One `annotation` occurrence per name, minus the predeclared ones and the
    /// names in `skip` (type parameters in scope, local types). The filter lives
    /// here so a type position and a whole function body share it.
    fn annotate_names(
        &mut self,
        names: Vec<TsNode<'_>>,
        enclosing_id: &str,
        skip: &HashSet<String>,
    ) {
        for name_node in names {
            let text = self.text(name_node);
            if is_predeclared_type(&text) || skip.contains(&text) {
                continue;
            }
            self.add_occurrence("annotation", name_node, enclosing_id);
        }
    }

    /// The constraints of a declaration's type parameters (`Stringer` in
    /// `func F[T Stringer]()` or `type G[T Stringer] []T`), enclosed by the
    /// declared node. The parameters themselves are skipped by
    /// `type_parameter_names`.
    fn record_constraints(&mut self, decl: TsNode<'_>, enclosing_id: &str) {
        let Some(params) = decl.child_by_field_name("type_parameters") else {
            return;
        };
        for declaration in walk_type(params, "type_parameter_declaration") {
            if let Some(constraint) = declaration.child_by_field_name("type") {
                self.record_annotation(constraint, enclosing_id);
            }
        }
    }

    /// Every type name inside a function body: composite-literal types, func
    /// literal parameters and results, local `var` types, `make`/`new`
    /// arguments, assertions, type switches, generic arguments. Enclosed by the
    /// callable, closures included (a closure's uses belong to the function that
    /// contains it, as calls do).
    ///
    /// Skipped, besides predeclared names: type parameters in scope; types
    /// declared inside the body (not nodes, they would become unlocated
    /// externals; scope-insensitive on purpose); and the base of `F[T](x)`,
    /// which tree-sitter reads as a `type_conversion_expression` over a
    /// `generic_type` although `F` is a function. Its type arguments stay.
    fn collect_body_types(&mut self, callable: TsNode<'_>, enclosing_id: &str) {
        let Some(body) = callable.child_by_field_name("body") else {
            return;
        };
        self.collect_scope_types(body, enclosing_id);
    }

    /// The type names of any scope (a function body, or the value of a
    /// package-level `var`), with the skips documented on `collect_body_types`.
    fn collect_scope_types(&mut self, body: TsNode<'_>, enclosing_id: &str) {
        let mut skip = type_parameter_names(self.source, body);
        let mut excluded: Vec<(usize, usize)> = Vec::new();
        for node in descendants(body) {
            match node.kind() {
                "type_spec" | "type_alias" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        skip.insert(self.text(name));
                    }
                }
                "type_conversion_expression" => {
                    if let Some(generic) = node.child_by_field_name("type")
                        && generic.kind() == "generic_type"
                        && let Some(base) = generic.child_by_field_name("type")
                    {
                        for id in walk_type(base, "type_identifier") {
                            excluded.push((id.start_byte(), id.end_byte()));
                        }
                    }
                }
                _ => {}
            }
        }
        let names: Vec<TsNode<'_>> = walk_type(body, "type_identifier")
            .into_iter()
            .filter(|n| !excluded.contains(&(n.start_byte(), n.end_byte())))
            .collect();
        self.annotate_names(names, enclosing_id, &skip);
        // The qualifier of `pkg.F[T](x)` is a real package use, so the base
        // exclusion above does not apply to it.
        self.record_type_qualifiers(body, enclosing_id);
    }

    /// PARAMETER nodes for a declaration's parameters, plus their annotations.
    ///
    /// Go allows several names per declaration (`a, b int`) and unnamed
    /// parameters in interfaces; both are handled by walking the names.
    fn collect_parameters(
        &mut self,
        callable: TsNode<'_>,
        callable_qname: &str,
        callable_id: &str,
    ) -> PyResult<()> {
        let Some(parameters) = callable.child_by_field_name("parameters") else {
            return Ok(());
        };
        for declaration in ts::children(parameters) {
            let variadic = declaration.kind() == "variadic_parameter_declaration";
            if declaration.kind() != "parameter_declaration" && !variadic {
                continue;
            }
            let type_node = declaration.child_by_field_name("type");
            let mut cursor = declaration.walk();
            let names: Vec<TsNode<'_>> = declaration
                .children_by_field_name("name", &mut cursor)
                .collect();
            for name_node in names {
                let name = self.text(name_node);
                if name.is_empty() {
                    continue;
                }
                let metadata = PyDict::new(self.py);
                metadata.set_item("annotation", type_node.map(|n| self.text(n)))?;
                metadata.set_item("name_span", ts::span(name_node))?;
                metadata.set_item("is_variadic", variadic)?;

                let qname = format!("{callable_qname}.{name}");
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
                    span: Some(ts::span(declaration)),
                    metadata: metadata.unbind(),
                };
                self.graph.insert_node(id.clone(), Py::new(self.py, node)?);
                self.push_relation(
                    callable_id.to_string(),
                    id.clone(),
                    RelationKind::Declares,
                )?;
                if let Some(type_node) = type_node {
                    self.record_annotation(type_node, &id);
                }
            }
        }
        Ok(())
    }

    /// The shared tail of a function and a method.
    fn on_callable(&mut self, node: TsNode<'_>, qname: &str, id: &str) -> PyResult<()> {
        self.collect_parameters(node, qname, id)?;
        if let Some(result) = node.child_by_field_name("result") {
            self.record_annotation(result, id);
        }
        self.collect_body_types(node, id);
        self.collect_calls(node, id);
        self.collect_references(node, id);
        self.collect_flow(node, qname, id)?;
        Ok(())
    }

    /// What the function's values hold, for the flow pass: locals that are called
    /// (`f := g; f()`), the names a function returns, and function-valued fields
    /// of struct literals (`S{fn: g}`).
    ///
    /// A local is a node only when it is called: nothing else needs one, and a
    /// node per local would multiply the graph for no reader.
    fn collect_flow(&mut self, callable: TsNode<'_>, qname: &str, id: &str) -> PyResult<()> {
        let Some(body) = callable.child_by_field_name("body") else {
            return Ok(());
        };
        let nodes = descendants(body);

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
                "index_expression" => function.child_by_field_name("operand").filter(|o| o.kind() == "identifier"),
                _ => None,
            };
            if let Some(base) = base {
                called.insert(self.text(base));
            }
        }

        // A local that is only a container the called one is read from - ranged
        // over, received from, indexed - holds the functions too.
        let mut relevant = called.clone();
        for _ in 0..3 {
            for node in &nodes {
                let (lefts, rights): (Vec<TsNode<'_>>, Vec<TsNode<'_>>) = match node.kind() {
                    "short_var_declaration" | "assignment_statement" => (
                        node.child_by_field_name("left").map(|l| ts::named_children(l)).unwrap_or_default(),
                        node.child_by_field_name("right").map(|r| ts::named_children(r)).unwrap_or_default(),
                    ),
                    "range_clause" => (
                        node.child_by_field_name("left").map(|l| ts::named_children(l)).unwrap_or_default(),
                        node.child_by_field_name("right").into_iter().collect(),
                    ),
                    _ => continue,
                };
                if !lefts.iter().any(|l| relevant.contains(&self.text(*l))) {
                    continue;
                }
                for right in rights {
                    let source = match right.kind() {
                        "identifier" => Some(right),
                        "index_expression" | "unary_expression" => {
                            right.child_by_field_name("operand").filter(|o| o.kind() == "identifier")
                        }
                        _ => None,
                    };
                    if let Some(source) = source {
                        relevant.insert(self.text(source));
                    }
                }
            }
        }
        let called = relevant;

        // Declarations first, so an assignment above a declaration still finds it.
        let mut locals: HashMap<String, String> = HashMap::new();
        for node in &nodes {
            let names: Vec<TsNode<'_>> = match node.kind() {
                "short_var_declaration" | "range_clause" => node
                    .child_by_field_name("left")
                    .map(|left| ts::named_children(left))
                    .unwrap_or_default(),
                "var_spec" => {
                    let mut cursor = node.walk();
                    node.children_by_field_name("name", &mut cursor).collect()
                }
                _ => continue,
            };
            for name_node in names {
                let name = self.text(name_node);
                if name_node.kind() != "identifier" || !called.contains(&name) || locals.contains_key(&name) {
                    continue;
                }
                let local_qname = format!("{qname}.{name}");
                let local_id = node_id(self.project_name, &local_qname, NodeKind::Variable.as_str());
                if !self.graph.has_node(&local_id) {
                    let metadata = PyDict::new(self.py);
                    metadata.set_item("name_span", ts::span(name_node))?;
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
        }

        for node in &nodes {
            match node.kind() {
                "short_var_declaration" | "assignment_statement" | "var_spec" => {
                    let (lefts, rights) = if node.kind() == "var_spec" {
                        let mut cursor = node.walk();
                        let names: Vec<_> = node.children_by_field_name("name", &mut cursor).collect();
                        let values = node.child_by_field_name("value").map(|v| ts::named_children(v)).unwrap_or_default();
                        (names, values)
                    } else {
                        // Only a plain `=` or `:=`: `+=` and friends assign no function.
                        if node.kind() == "assignment_statement"
                            && node.child_by_field_name("operator").is_some_and(|op| self.text(op) != "=")
                        {
                            continue;
                        }
                        (
                            node.child_by_field_name("left").map(|l| ts::named_children(l)).unwrap_or_default(),
                            node.child_by_field_name("right").map(|r| ts::named_children(r)).unwrap_or_default(),
                        )
                    };
                    if lefts.len() != rights.len() {
                        continue;
                    }
                    for (left, right) in lefts.into_iter().zip(rights) {
                        let Some(var_id) = locals.get(&self.text(left)).cloned() else {
                            continue;
                        };
                        self.record_aliases(right, &var_id, 0);
                    }
                }
                // `for _, f := range fns`: the loop variable holds what the container holds.
                "range_clause" => {
                    let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right"))
                    else {
                        continue;
                    };
                    for variable in ts::named_children(left) {
                        if let Some(var_id) = locals.get(&self.text(variable)).cloned() {
                            self.record_aliases(right, &var_id, 0);
                        }
                    }
                }
                // `ch <- f`: the channel holds `f`.
                "send_statement" => {
                    let (Some(channel), Some(value)) = (node.child_by_field_name("channel"), node.child_by_field_name("value"))
                    else {
                        continue;
                    };
                    if let Some(var_id) = locals.get(&self.text(channel)).cloned() {
                        self.record_aliases(value, &var_id, 0);
                    }
                }
                "return_statement" => {
                    for value in ts::named_children(*node).into_iter().flat_map(|l| {
                        if l.kind() == "expression_list" { ts::named_children(l) } else { vec![l] }
                    }) {
                        if let Some(name) = plain_name(value) {
                            self.add_occurrence_with("alias", name, id, Some("return".into()));
                        }
                    }
                }
                "composite_literal" => self.struct_literal_fields(*node),
                _ => {}
            }
        }
        Ok(())
    }

    /// What `value` hands to the variable it is assigned to.
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
            "index_expression" => {
                if let Some(base) = value.child_by_field_name("operand").and_then(plain_name) {
                    self.add_occurrence_with("alias", base, variable_id, None);
                }
            }
            "unary_expression" if depth < 3 => {
                // &x, <-ch: what is taken from is what is held.
                if let Some(operand) = value.child_by_field_name("operand") {
                    self.record_aliases(operand, variable_id, depth + 1);
                }
            }
            "composite_literal" if depth < 3 => {
                if let Some(body) = value.child_by_field_name("body") {
                    for element in ts::named_children(body) {
                        let element = match element.kind() {
                            "keyed_element" => ts::named_children(element).last().copied(),
                            _ => Some(element),
                        };
                        // literal_element wraps its expression.
                        let element = element.and_then(|e| {
                            if e.kind() == "literal_element" { ts::named_children(e).first().copied() } else { Some(e) }
                        });
                        if let Some(element) = element {
                            self.record_aliases(element, variable_id, depth + 1);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// `S{fn: g}`: the field `S.fn` holds what `g` resolves to.
    fn struct_literal_fields(&mut self, literal: TsNode<'_>) {
        let Some(type_name) = type_name_node(literal.child_by_field_name("type")) else {
            return;
        };
        let type_name = self.text(type_name);
        let Some(body) = literal.child_by_field_name("body") else {
            return;
        };
        for element in ts::named_children(body) {
            if element.kind() != "keyed_element" {
                continue;
            }
            let parts = ts::named_children(element);
            let (Some(key), Some(value)) = (parts.first().copied(), parts.last().copied()) else {
                continue;
            };
            let key = if key.kind() == "literal_element" { ts::named_children(key).first().copied() } else { Some(key) };
            let value = if value.kind() == "literal_element" { ts::named_children(value).first().copied() } else { Some(value) };
            let (Some(key), Some(value)) = (key, value) else {
                continue;
            };
            if key.kind() != "identifier" {
                continue;
            }
            let field_qname = format!("{}.{type_name}.{}", self.package_qname, self.text(key));
            let field_id = node_id(self.project_name, &field_qname, NodeKind::Attribute.as_str());
            self.record_aliases(value, &field_id, 0);
        }
    }

    /// ATTRIBUTE nodes for a struct's fields, plus their annotations.
    ///
    /// An embedded field is an ATTRIBUTE named after its type's last identifier,
    /// which is the position go/types gives the field, so a selector, a keyed
    /// literal key or an argument that uses it binds to a node. `collect_bases`
    /// still records the embedded type as a `base` occurrence.
    fn collect_fields(
        &mut self,
        type_node: TsNode<'_>,
        type_qname: &str,
        type_id: &str,
    ) -> PyResult<()> {
        for list in ts::children(type_node) {
            if list.kind() != "field_declaration_list" {
                continue;
            }
            for field in ts::children(list) {
                if field.kind() != "field_declaration" {
                    continue;
                }
                if let Some(name_node) = field.child_by_field_name("name") {
                    let name = self.text(name_node);
                    let qname = format!("{type_qname}.{name}");
                    let id = self.declare(&qname, &name, NodeKind::Attribute, field, name_node)?;
                    self.push_relation(type_id.to_string(), id.clone(), RelationKind::Declares)?;
                    if let Some(field_type) = field.child_by_field_name("type") {
                        self.record_annotation(field_type, &id);
                    }
                } else {
                    let Some(field_type) = field.child_by_field_name("type") else {
                        continue;
                    };
                    let Some(name_node) = embedded_name_node(Some(field_type)) else {
                        continue;
                    };
                    let name = self.text(name_node);
                    let qname = format!("{type_qname}.{name}");
                    let id = self.declare(&qname, &name, NodeKind::Attribute, field, name_node)?;
                    self.push_relation(type_id.to_string(), id.clone(), RelationKind::Declares)?;
                    // Names only: `collect_bases` already records the package qualifier
                    // with the class as its source.
                    let params = type_parameter_names(self.source, field_type);
                    self.annotate_names(walk_type(field_type, "type_identifier"), &id, &params);
                }
            }
        }
        Ok(())
    }

    /// METHOD nodes for the methods an interface declares: a call through an
    /// interface lands on the interface's method, and without a node for it the
    /// call went nowhere.
    fn collect_interface_methods(
        &mut self,
        type_node: TsNode<'_>,
        type_qname: &str,
        type_id: &str,
    ) -> PyResult<()> {
        if type_node.kind() != "interface_type" {
            return Ok(());
        }
        for element in ts::children(type_node) {
            if element.kind() != "method_elem" {
                continue;
            }
            let Some(name_node) = element.child_by_field_name("name") else {
                continue;
            };
            let name = self.text(name_node);
            let qname = format!("{type_qname}.{name}");
            let id = self.declare(&qname, &name, NodeKind::Method, element, name_node)?;
            self.push_relation(type_id.to_string(), id.clone(), RelationKind::Declares)?;
            self.collect_parameters(element, &qname, &id)?;
            if let Some(result) = element.child_by_field_name("result") {
                self.record_annotation(result, &id);
            }
        }
        Ok(())
    }

    fn on_function(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.package_qname);
        let id = self.declare(&qname, &name, NodeKind::Function, node, name_node)?;
        self.record_constraints(node, &id);
        self.on_callable(node, &qname, &id)
    }

    fn on_method(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let receiver = self.receiver_type(node);
        let prefix = if receiver.is_empty() {
            String::new()
        } else {
            format!("{receiver}.")
        };
        let qname = format!("{}.{prefix}{name}", self.package_qname);
        let id = self.declare(&qname, &name, NodeKind::Method, node, name_node)?;
        // The receiver type is a use of the type; the method is the enclosing node
        // (collect_parameters reads only `parameters`, so no PARAMETER exists).
        if let Some(receiver_list) = node.child_by_field_name("receiver") {
            for declaration in ts::children(receiver_list) {
                if declaration.kind() == "parameter_declaration"
                    && let Some(receiver_type) = declaration.child_by_field_name("type")
                {
                    self.record_annotation(receiver_type, &id);
                }
            }
        }
        self.on_callable(node, &qname, &id)
    }

    fn on_type_declaration(&mut self, node: TsNode<'_>) -> PyResult<()> {
        for spec in ts::children(node) {
            if !matches!(spec.kind(), "type_spec" | "type_alias") {
                continue;
            }
            let Some(name_node) = spec.child_by_field_name("name") else {
                continue;
            };
            let type_node = spec.child_by_field_name("type");
            let is_struct_like = spec.kind() == "type_spec"
                && type_node.is_some_and(|t| matches!(t.kind(), "struct_type" | "interface_type"));
            let kind = if is_struct_like { NodeKind::Class } else { NodeKind::TypeAlias };

            let name = self.text(name_node);
            let qname = format!("{}.{name}", self.package_qname);
            let id = self.declare(&qname, &name, kind, spec, name_node)?;
            self.record_constraints(spec, &id);

            if is_struct_like
                && let Some(type_node) = type_node
            {
                self.collect_bases(type_node, &id);
                self.collect_fields(type_node, &qname, &id)?;
                self.collect_interface_methods(type_node, &qname, &id)?;
            } else if let Some(type_node) = type_node {
                // `type Items []Item`, `type H func(*Item)`, any alias: the right-hand
                // side is a use of the types it names.
                self.record_annotation(type_node, &id);
            }
        }
        Ok(())
    }

    /// Scans one package-level initializer with the VARIABLE as the enclosing
    /// definition. A func literal that is the whole value is the variable's body:
    /// its parameters become PARAMETER nodes `<var>.<p>` and its called locals are
    /// nodes under the variable. Any other value is walked like a function body, so
    /// a literal nested in it is a closure (no PARAMETER nodes, its calls belong to
    /// the variable).
    fn scan_initializer(&mut self, value: TsNode<'_>, var_qname: &str, var_id: &str) -> PyResult<()> {
        if value.kind() == "func_literal" {
            return self.on_callable(value, var_qname, var_id);
        }
        self.collect_scope_types(value, var_id);
        self.collect_calls(value, var_id);
        self.collect_references(value, var_id);
        Ok(())
    }

    /// Declares each name of a `var`/`const` spec, then scans the values. With one
    /// value per name, value i belongs to name i; otherwise (`var a, b = f()`) every
    /// value is scanned once, by the first name, so no site gets two occurrences.
    /// A const with an implicit repeated value has no value node: nothing to scan.
    fn on_value_specs(&mut self, node: TsNode<'_>, spec_kind: &str) -> PyResult<()> {
        for spec in walk_type(node, spec_kind) {
            let mut cursor = spec.walk();
            let names: Vec<TsNode<'_>> = spec
                .children_by_field_name("name", &mut cursor)
                .collect();
            let values: Vec<TsNode<'_>> = spec
                .child_by_field_name("value")
                .map(|v| ts::named_children(v))
                .unwrap_or_default();
            let mut declared: Vec<(String, String)> = Vec::new();
            for (position, name_node) in names.iter().enumerate() {
                let name = self.text(*name_node);
                let qname = format!("{}.{name}", self.package_qname);
                let id = self.declare(&qname, &name, NodeKind::Variable, spec, *name_node)?;
                // `var Global Item`, `const K Kind = 1`: one annotation per name.
                if let Some(declared_type) = spec.child_by_field_name("type") {
                    self.record_annotation(declared_type, &id);
                }
                // `var handler = g`: the variable holds what it is initialised with.
                if spec_kind == "var_spec"
                    && values.len() == names.len()
                    && let Some(value) = values.get(position)
                {
                    self.record_aliases(*value, &id, 0);
                }
                declared.push((qname, id));
            }
            if declared.is_empty() {
                continue;
            }
            if values.len() == names.len() {
                for (value, (qname, id)) in values.iter().zip(declared.iter()) {
                    self.scan_initializer(*value, qname, id)?;
                }
            } else {
                let (qname, id) = &declared[0];
                for value in &values {
                    self.scan_initializer(*value, qname, id)?;
                }
            }
        }
        Ok(())
    }

    fn on_import_declaration(&mut self, node: TsNode<'_>) -> PyResult<()> {
        for spec in walk_type(node, "import_spec") {
            let import_path = spec
                .child_by_field_name("path")
                .map(|p| self.text(p).trim_matches('"').to_string())
                .unwrap_or_default();
            if import_path.is_empty() {
                continue;
            }
            let origin = classify_import(&import_path, self.module_path, self.required);
            self.declare_import(spec, &import_path, origin)?;
        }
        Ok(())
    }

    fn declare_import(
        &mut self,
        spec: TsNode<'_>,
        import_path: &str,
        origin: &str,
    ) -> PyResult<()> {
        let import_qname = format!("{}::{import_path}", self.file_rel);
        let import_id = node_id(self.project_name, &import_qname, NodeKind::Import.as_str());
        if self.graph.has_node(&import_id) {
            return Ok(());
        }
        // The path literal, quotes included: where this import names its package.
        let path_span = spec.child_by_field_name("path").map(ts::span).unwrap_or_else(|| ts::span(spec));

        let metadata = PyDict::new(self.py);
        metadata.set_item("origin", origin)?;
        metadata.set_item("import_path", import_path)?;
        let node = Node {
            id: import_id.clone(),
            kind: NodeKind::Import,
            qualified_name: import_qname,
            name: import_path.to_string(),
            file_path: Some(self.file_rel.to_string()),
            span: Some(ts::span(spec)),
            metadata: metadata.unbind(),
        };
        self.graph.insert_node(import_id.clone(), Py::new(self.py, node)?);
        self.push_relation(
            self.file_id.to_string(),
            import_id.clone(),
            RelationKind::Declares,
        )?;

        if origin == "internal" {
            // Deferred: bound to the real MODULE node once every package
            // exists (falling back to an EXTERNAL_SYMBOL otherwise). IMPORTS
            // is deferred with it, since it needs the same target.
            self.internal_imports.push((
                import_id,
                import_path.to_string(),
                self.file_id.to_string(),
                path_span,
            ));
            return Ok(());
        }

        let sym_id = node_id(self.project_name, import_path, NodeKind::ExternalSymbol.as_str());
        if !self.graph.has_node(&sym_id) {
            let metadata = PyDict::new(self.py);
            metadata.set_item("origin", origin)?;
            let node = Node {
                id: sym_id.clone(),
                kind: NodeKind::ExternalSymbol,
                qualified_name: import_path.to_string(),
                name: import_path.rsplit('/').next().unwrap_or(import_path).to_string(),
                file_path: None,
                span: None,
                metadata: metadata.unbind(),
            };
            self.graph.insert_node(sym_id.clone(), Py::new(self.py, node)?);
        }
        self.push_relation(self.file_id.to_string(), sym_id.clone(), RelationKind::Imports)?;
        self.push_relation_with_span(import_id, sym_id, RelationKind::ResolvesTo, path_span)
    }

    fn receiver_type(&self, method_node: TsNode<'_>) -> String {
        let Some(receiver) = method_node.child_by_field_name("receiver") else {
            return String::new();
        };
        // The receiver's own type: the leftmost name, `Box` in `b Box[T]`, not a
        // type parameter after it.
        walk_type(receiver, "type_identifier")
            .into_iter()
            .min_by_key(|n| n.start_byte())
            .map(|n| self.text(n))
            .unwrap_or_default()
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

    /// A structural edge that records where it was observed (so it is exempt
    /// from `dedupe_structural_relations`).
    fn push_relation_with_span(
        &mut self,
        source_id: String,
        target_id: String,
        kind: RelationKind,
        span: Span,
    ) -> PyResult<()> {
        let metadata = PyDict::new(self.py);
        metadata.set_item("span", span)?;
        let relation = Relation { source_id, target_id, kind, metadata: metadata.unbind() };
        self.graph.push_relation(Py::new(self.py, relation)?);
        Ok(())
    }
}

/// Classification of a Go import path.
///
/// Go's own rule applies: a standard-library path has no dot in its first
/// segment, while third-party paths start with a domain.
pub fn classify_import(
    import_path: &str,
    module_path: Option<&str>,
    required: &HashSet<String>,
) -> &'static str {
    if let Some(module_path) = module_path
        && (import_path == module_path
            || import_path.starts_with(&format!("{module_path}/")))
    {
        return "internal";
    }
    let first = import_path.split('/').next().unwrap_or(import_path);
    if !first.contains('.') {
        return "stdlib";
    }
    for req in required {
        if import_path == req || import_path.starts_with(&format!("{req}/")) {
            return "third_party";
        }
    }
    "unknown"
}

/// Standard-library functions and methods that call a function they are given,
/// by the callee as written (`sort.Slice`, `once.Do`). The list is closed on
/// purpose: a function handed to anything else is a reference, since whether the
/// callee calls it cannot be known from the program. It applies only where the
/// callee is not a function of the project.
fn calls_its_arguments(callee: &str) -> bool {
    const FUNCTIONS: [&str; 33] = [
        "sort.Slice", "sort.SliceStable", "sort.SliceIsSorted", "sort.Search", "slices.SortFunc",
        "slices.SortStableFunc", "slices.IndexFunc", "slices.ContainsFunc", "slices.DeleteFunc",
        "slices.BinarySearchFunc", "slices.MaxFunc", "slices.MinFunc", "slices.EqualFunc",
        "slices.IsSortedFunc", "maps.DeleteFunc", "strings.Map", "strings.FieldsFunc",
        "strings.IndexFunc", "strings.LastIndexFunc", "strings.TrimFunc", "strings.TrimLeftFunc",
        "strings.TrimRightFunc", "bytes.Map", "bytes.FieldsFunc", "bytes.IndexFunc",
        "http.HandleFunc", "time.AfterFunc", "filepath.Walk", "filepath.WalkDir", "sync.OnceFunc",
        "sync.OnceValue", "context.AfterFunc", "runtime.SetFinalizer",
    ];
    if FUNCTIONS.contains(&callee) {
        return true;
    }
    // Methods, by name, whatever the receiver: `once.Do(f)`, `g.Go(f)`, `t.Run(name, f)`,
    // `mux.HandleFunc(path, f)`.
    callee
        .rsplit_once('.')
        .is_some_and(|(_, name)| matches!(name, "Do" | "Go" | "Run" | "AfterFunc" | "HandleFunc"))
}

#[cfg(test)]
mod tests {
    use super::assumed_package_name;

    #[test]
    fn assumed_names_follow_gopls() {
        assert_eq!(assumed_package_name("fmt"), "fmt");
        assert_eq!(assumed_package_name("example.com/m/util"), "util");
        assert_eq!(assumed_package_name("gopkg.in/yaml.v3"), "yaml");
        assert_eq!(assumed_package_name("github.com/go-chi/chi/v5"), "chi");
        assert_eq!(assumed_package_name("github.com/mattn/go-sqlite3"), "sqlite3");
        assert_eq!(assumed_package_name("github.com/x/v"), "v");
    }
}
