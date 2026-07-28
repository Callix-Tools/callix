//! Structural extraction from Go: packages, types, functions, imports.
//!
//! Simpler than Python and TypeScript: Go has no nested declaration scopes,
//! so there are no stacks — only a file's top-level declarations are walked.

use std::collections::HashSet;

use pyo3::prelude::*;
use pyo3::types::PyDict;
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
    /// `(import node id, path)` for internal imports: the adapter substitutes
    /// the real MODULE node once every package exists.
    internal_imports: Vec<(String, String)>,
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

    pub fn into_parts(self) -> (Vec<OccurrenceRef>, Vec<(String, String)>) {
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
        let span = ts::span(name_node);
        self.occurrences.push(OccurrenceRef {
            role: role.to_string(),
            line: span.start_line,
            col: span.start_col,
            enclosing_id: enclosing_id.to_string(),
            span,
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
            if let Some(name_node) = called_name(function) {
                self.add_occurrence("call", name_node, enclosing_id);
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

    fn on_function(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.package_qname);
        let id = self.declare(&qname, &name, NodeKind::Function, node, name_node)?;
        self.collect_calls(node, &id);
        self.collect_references(node, &id);
        Ok(())
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
        self.collect_calls(node, &id);
        self.collect_references(node, &id);
        Ok(())
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
            for name_node in names {
                let name = self.text(name_node);
                let qname = format!("{}.{name}", self.package_qname);
                self.declare(&qname, &name, NodeKind::Variable, spec, name_node)?;
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
            // exists (falling back to an EXTERNAL_SYMBOL otherwise).
            self.internal_imports
                .push((import_id, import_path.to_string()));
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
        self.push_relation(import_id, sym_id, RelationKind::ResolvesTo)
    }

    fn receiver_type(&self, method_node: TsNode<'_>) -> String {
        let Some(receiver) = method_node.child_by_field_name("receiver") else {
            return String::new();
        };
        walk_type(receiver, "type_identifier")
            .first()
            .map(|n| self.text(*n))
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
