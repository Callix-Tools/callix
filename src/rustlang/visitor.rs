//! Structural extraction from Rust: modules, types, functions, imports.
//!
//! Unlike Go, `impl` blocks are dispatched in a second pass. Rust does not
//! require a type or trait to be declared above its `impl`, and without
//! deferring, an `impl Trait for Type` written above `struct Type` would find
//! no type node and lose the `base` occurrence.

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

/// A node's descendants (itself included) in the order Python walks them: a
/// stack popped from the end, i.e. depth-first and right to left.
fn descendants<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        out.push(current);
        stack.extend(ts::children(current));
    }
    out
}

/// Every descendant of the given kind; we do not descend into a match.
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

/// The leftmost node of a set — the leading name of a compound type.
fn leftmost<'tree>(nodes: &[TsNode<'tree>]) -> Option<TsNode<'tree>> {
    nodes.iter().min_by_key(|n| n.start_byte()).copied()
}

/// The callee's name token; None when an expression result is called.
fn called_name<'tree>(function: TsNode<'tree>) -> Option<TsNode<'tree>> {
    match function.kind() {
        // foo()
        "identifier" => Some(function),
        // util::helper() / Type::assoc()
        "scoped_identifier" => function.child_by_field_name("name"),
        // obj.method()
        "field_expression" => function.child_by_field_name("field"),
        // calling an expression result, e.g. funcs[0]()
        _ => None,
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
    /// `(import node id, path, importing module)` for internal imports: the
    /// adapter substitutes the real MODULE node once every module exists.
    internal_imports: Vec<(String, String, String)>,
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
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn into_parts(self) -> (Vec<OccurrenceRef>, Vec<(String, String, String)>) {
        (self.occurrences, self.internal_imports)
    }

    fn text(&self, node: TsNode<'_>) -> String {
        ts::text(node, self.source).into_owned()
    }

    fn text_or_empty(&self, node: Option<TsNode<'_>>) -> String {
        node.map(|n| self.text(n)).unwrap_or_default()
    }

    pub fn extract(&mut self, root: TsNode<'_>) -> PyResult<()> {
        self.dispatch(root)
    }

    /// Dispatches direct children to their handlers; `impl` in a second pass.
    fn dispatch(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let mut deferred_impls = Vec::new();
        for child in ts::children(node) {
            match child.kind() {
                "impl_item" => deferred_impls.push(child),
                "mod_item" => self.on_mod_item(child)?,
                "function_item" => self.on_function_item(child)?,
                "struct_item" | "enum_item" | "trait_item" | "union_item" => {
                    self.declare_named(child, NodeKind::Class)?;
                }
                "type_item" => {
                    self.declare_named(child, NodeKind::TypeAlias)?;
                }
                "const_item" | "static_item" => {
                    self.declare_named(child, NodeKind::Variable)?;
                }
                "use_declaration" => self.on_use_declaration(child)?,
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
    fn on_mod_item(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let (Some(name_node), Some(body)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("body"),
        ) else {
            return Ok(());
        };
        let inner = format!("{}::{}", self.module_qname, self.text(name_node));
        let outer = std::mem::replace(&mut self.module_qname, inner);
        let result = self.dispatch(body);
        self.module_qname = outer;
        result
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

    fn declare_named(&mut self, node: TsNode<'_>, kind: NodeKind) -> PyResult<Option<String>> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(None);
        };
        let name = self.text(name_node);
        let qname = format!("{}::{name}", self.module_qname);
        self.declare(&qname, &name, kind, node, name_node).map(Some)
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

    fn on_function_item(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}::{name}", self.module_qname);
        let id = self.declare(&qname, &name, NodeKind::Function, node, name_node)?;
        self.collect_calls(node, &id);
        Ok(())
    }

    fn on_impl_item(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let type_name = self.impl_type_name(node);
        self.collect_impl_trait(node, &type_name)?;
        let Some(body) = node.child_by_field_name("body") else {
            return Ok(());
        };
        for item in ts::children(body) {
            if item.kind() != "function_item" {
                continue;
            }
            let Some(name_node) = item.child_by_field_name("name") else {
                continue;
            };
            let name = self.text(name_node);
            let prefix = if type_name.is_empty() {
                String::new()
            } else {
                format!("{type_name}.")
            };
            let qname = format!("{}::{prefix}{name}", self.module_qname);
            let id = self.declare(&qname, &name, NodeKind::Method, item, name_node)?;
            self.collect_calls(item, &id);
        }
        Ok(())
    }

    /// `impl Trait for Type` as a `base` occurrence.
    ///
    /// Rust declares this relationship on a separate `impl` rather than on the
    /// type itself (unlike Python's and Go's base lists), so the implementing
    /// type's id has to be recomputed here instead of reused from
    /// `declare_named`.
    fn collect_impl_trait(&mut self, impl_node: TsNode<'_>, type_name: &str) -> PyResult<()> {
        let Some(trait_node) = impl_node.child_by_field_name("trait") else {
            return Ok(());
        };
        if type_name.is_empty() {
            return Ok(());
        }
        let idents = walk_type(trait_node, "type_identifier");
        let trait_name_node = leftmost(&idents).unwrap_or(trait_node);

        let type_qname = format!("{}::{type_name}", self.module_qname);
        let type_id = node_id(self.project_name, &type_qname, NodeKind::Class.as_str());
        if !self.graph.has_node(&type_id) {
            return Ok(());
        }
        self.add_occurrence("base", trait_name_node, &type_id);
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
        let import_path = self.text_or_empty(node.child_by_field_name("argument"));
        if import_path.is_empty() {
            return Ok(());
        }
        let origin = super::deps::classify_import(&import_path, self.crate_name, self.deps);
        self.declare_import(node, &import_path, origin)
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
            // exists (falling back to an EXTERNAL_SYMBOL otherwise).
            self.internal_imports.push((
                import_id,
                import_path.to_string(),
                self.module_qname.clone(),
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
