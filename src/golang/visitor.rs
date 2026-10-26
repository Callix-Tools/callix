//! Structural extraction from Go: packages, types, functions, imports.
//!
//! Simpler than Python and TypeScript: Go has no nested declaration scopes,
//! so there are no stacks — only a file's top-level declarations are walked.

use std::collections::{HashMap, HashSet};

use crate::py::prelude::*;
use crate::py::types::PyDict;
use tree_sitter::Node as TsNode;

use crate::graph::Graph;
use crate::ids::node_id;
use crate::node::{Node, NodeKind};
use crate::occurrence::OccurrenceRef;
use crate::relation::{Relation, RelationKind};
use crate::ts;

/// A node's descendants in the order the Python version walks them: a stack
/// popped from the end, i.e. depth-first and right to left.
fn descendants<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        out.push(current);
        stack.extend(ts::children(current));
    }
    out
}

/// Every descendant of the given kind, in the same traversal order.
fn walk_type<'tree>(node: TsNode<'tree>, kind: &str) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = ts::children(node);
    while let Some(current) = stack.pop() {
        if current.kind() == kind {
            out.push(current);
        } else {
            stack.extend(ts::children(current));
        }
    }
    out
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
            grandparent.kind() == "assignment_statement"
                && grandparent.child_by_field_name("left") == Some(parent)
        });
    }
    false
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
    /// `(import node id, path, importing file)` for internal imports: the
    /// adapter substitutes the real MODULE node once every package exists, and
    /// emits IMPORTS from the file at the same time.
    internal_imports: Vec<(String, String, String)>,
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
        }
    }

    pub fn into_parts(self) -> (Vec<OccurrenceRef>, Vec<(String, String, String)>) {
        (self.occurrences, self.internal_imports)
    }

    fn text(&self, node: TsNode<'_>) -> String {
        ts::text(node, self.source).into_owned()
    }

    /// Parses the file's top-level declarations.
    pub fn extract(&mut self, root: TsNode<'_>) -> PyResult<()> {
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
        self.occurrences.push(OccurrenceRef {
            role: role.to_string(),
            line: span.start_line,
            col: span.start_col,
            enclosing_id: enclosing_id.to_string(),
            span,
            detail,
        });
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
    fn collect_references(&mut self, scope: TsNode<'_>, enclosing_id: &str) {
        for node in descendants(scope) {
            if node.kind() != "selector_expression" {
                continue;
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
                    if let Some(name_node) = type_name_node(fd.child_by_field_name("type")) {
                        self.add_occurrence("base", name_node, enclosing_id);
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
        }
    }

    /// An `annotation` for the leading identifier of a type.
    ///
    /// `*S`, `[]S` and `pkg.S` all reduce to the `type_identifier` the
    /// resolver can bind; a built-in like `string` has none and is skipped.
    fn record_annotation(&mut self, type_node: TsNode<'_>, enclosing_id: &str) {
        if let Some(name_node) = walk_type(type_node, "type_identifier").first() {
            self.add_occurrence("annotation", *name_node, enclosing_id);
        }
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

    /// ATTRIBUTE nodes for a struct's named fields, plus their annotations.
    ///
    /// A field with no name is an embedded type, which `collect_bases` already
    /// records as a `base` occurrence.
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
                let Some(name_node) = field.child_by_field_name("name") else {
                    continue;
                };
                let name = self.text(name_node);
                let qname = format!("{type_qname}.{name}");
                let id = self.declare(&qname, &name, NodeKind::Attribute, field, name_node)?;
                self.push_relation(type_id.to_string(), id.clone(), RelationKind::Declares)?;
                if let Some(field_type) = field.child_by_field_name("type") {
                    self.record_annotation(field_type, &id);
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

            if is_struct_like
                && let Some(type_node) = type_node
            {
                self.collect_bases(type_node, &id);
                self.collect_fields(type_node, &qname, &id)?;
                self.collect_interface_methods(type_node, &qname, &id)?;
            }
        }
        Ok(())
    }

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
            for (position, name_node) in names.iter().enumerate() {
                let name = self.text(*name_node);
                let qname = format!("{}.{name}", self.package_qname);
                let id = self.declare(&qname, &name, NodeKind::Variable, spec, *name_node)?;
                // `var handler = g`: the variable holds what it is initialised with.
                if spec_kind == "var_spec"
                    && values.len() == names.len()
                    && let Some(value) = values.get(position)
                {
                    self.record_aliases(*value, &id, 0);
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
            self.internal_imports
                .push((import_id, import_path.to_string(), self.file_id.to_string()));
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
        self.push_relation(import_id, sym_id, RelationKind::ResolvesTo)
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
