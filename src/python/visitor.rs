//! Walking Python's CST and filling the graph.
//!
//! The state is three stacks, pushed and popped as scopes change:
//! `scope_stack` (the qualified-name prefix), `container_stack` (the parent's
//! id), and `kind_stack` (to tell METHOD from FUNCTION).
//!
//! The visitor creates no CALLS/INHERITS_FROM/REFERENCES/HAS_TYPE edges — it
//! collects `OccurrenceRef`s, which the resolution pass binds to definitions.

use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use tree_sitter::Node as TsNode;

use crate::graph::Graph;
use crate::ids::node_id;
use crate::node::{Node, NodeKind};
use crate::occurrence::{ImportClassifier, OccurrenceRef};
use crate::relation::{Relation, RelationKind};
use crate::ts;

use super::resolve_relative_import;

const NESTED_DEF_TYPES: [&str; 3] = [
    "function_definition",
    "class_definition",
    "decorated_definition",
];

const ENUM_BASES: [&str; 5] = ["Enum", "IntEnum", "StrEnum", "Flag", "IntFlag"];

/// The equivalent of `str.isupper()`: there are letters and all are upper.
fn is_upper(name: &str) -> bool {
    let mut has_cased = false;
    for ch in name.chars() {
        if ch.is_alphabetic() {
            has_cased = true;
            if !ch.is_uppercase() {
                return false;
            }
        }
    }
    has_cased
}

pub struct PythonVisitor<'a> {
    py: Python<'a>,
    graph: &'a mut Graph,
    source: &'a [u8],
    project_name: &'a str,
    file_path: &'a str,
    file_node_id: &'a str,
    module_qname: &'a str,
    classifier: &'a ImportClassifier,
    module_index: HashMap<String, String>,
    scope_stack: Vec<String>,
    container_stack: Vec<String>,
    kind_stack: Vec<NodeKind>,
    occurrences: Vec<OccurrenceRef>,
}

impl<'a> PythonVisitor<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        py: Python<'a>,
        graph: &'a mut Graph,
        source: &'a [u8],
        project_name: &'a str,
        file_path: &'a str,
        module_qname: &'a str,
        file_node_id: &'a str,
        classifier: &'a ImportClassifier,
    ) -> Self {
        let module_index = graph
            .module_index()
            .into_iter()
            .map(|(qname, id)| (qname.to_string(), id.to_string()))
            .collect();
        Self {
            py,
            graph,
            source,
            project_name,
            file_path,
            file_node_id,
            module_qname,
            classifier,
            module_index,
            scope_stack: vec![module_qname.to_string()],
            container_stack: vec![file_node_id.to_string()],
            kind_stack: vec![NodeKind::File],
            occurrences: Vec::new(),
        }
    }

    pub fn into_occurrences(self) -> Vec<OccurrenceRef> {
        self.occurrences
    }

    fn text(&self, node: TsNode<'_>) -> String {
        ts::text(node, self.source).into_owned()
    }

    fn scope(&self) -> &str {
        self.scope_stack.last().map(String::as_str).unwrap_or("")
    }

    fn container(&self) -> String {
        self.container_stack.last().cloned().unwrap_or_default()
    }

    // -- dispatch ---------------------------------------------------------

    pub fn visit(&mut self, node: TsNode<'_>) -> PyResult<()> {
        match node.kind() {
            "module" => self.visit_children(node),
            "decorated_definition" => self.visit_decorated_definition(node),
            "class_definition" => self.handle_class(node, &[], &[]),
            "function_definition" => self.handle_function(node, &[], &[]),
            "import_statement" => self.visit_import_statement(node),
            "import_from_statement" => self.visit_import_from_statement(node),
            "expression_statement" => self.visit_expression_statement(node),
            "return_statement" => self.visit_return_statement(node),
            _ => self.visit_children(node),
        }
    }

    fn visit_children(&mut self, node: TsNode<'_>) -> PyResult<()> {
        for child in ts::children(node) {
            self.visit(child)?;
        }
        Ok(())
    }

    fn visit_decorated_definition(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let decorator_nodes: Vec<_> = ts::children(node)
            .into_iter()
            .filter(|c| c.kind() == "decorator")
            .collect();
        let decorators: Vec<String> = decorator_nodes
            .iter()
            .map(|c| self.decorator_name(*c))
            .collect();

        let Some(inner) =
            ts::child_of_types(node, &["class_definition", "function_definition"])
        else {
            return Ok(());
        };
        if inner.kind() == "class_definition" {
            self.handle_class(inner, &decorators, &decorator_nodes)
        } else {
            self.handle_function(inner, &decorators, &decorator_nodes)
        }
    }

    // -- imports ----------------------------------------------------------

    fn visit_import_statement(&mut self, node: TsNode<'_>) -> PyResult<()> {
        // import X / import X.Y / import X as Y
        for child in ts::children(node) {
            match child.kind() {
                "dotted_name" => {
                    let name = self.dotted_name(child);
                    self.emit_import(&name, &name, false, 0, None, false)?;
                }
                "aliased_import" => {
                    let Some(name_node) = ts::child_of_type(child, "dotted_name") else {
                        continue;
                    };
                    let alias_node = ts::child_of_type(child, "identifier");
                    let name = self.dotted_name(name_node);
                    let local = match alias_node {
                        Some(node) => self.text(node),
                        None => name.clone(),
                    };
                    let alias = alias_node.map(|_| local.clone());
                    self.emit_import(&local, &name, false, 0, alias.as_deref(), false)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn visit_import_from_statement(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let children = ts::children(node);

        // The source module and relative level come only before `import`.
        let mut level = 0usize;
        let mut source_module = String::new();
        for child in &children {
            if child.kind() == "import" {
                break;
            }
            match child.kind() {
                "relative_import" => {
                    if let Some(prefix) = ts::child_of_type(*child, "import_prefix") {
                        level = self.text(prefix).matches('.').count();
                    }
                    let mod_name = ts::child_of_type(*child, "dotted_name")
                        .map(|n| self.dotted_name(n));
                    source_module = resolve_relative_import(
                        self.module_qname,
                        level,
                        mod_name.as_deref(),
                    );
                }
                "dotted_name" => source_module = self.dotted_name(*child),
                _ => {}
            }
        }

        let is_relative = level > 0;
        let qualify = |imported: &str| {
            if source_module.is_empty() {
                imported.to_string()
            } else {
                format!("{source_module}.{imported}")
            }
        };

        // The names after `import`.
        let mut past_import_kw = false;
        for child in &children {
            if child.kind() == "import" {
                past_import_kw = true;
                continue;
            }
            if !past_import_kw {
                continue;
            }
            match child.kind() {
                "dotted_name" => {
                    let imported = self.dotted_name(*child);
                    let ext_qname = qualify(&imported);
                    self.emit_import(&imported, &ext_qname, is_relative, level, None, false)?;
                }
                "aliased_import" => {
                    let Some(name_node) = ts::child_of_type(*child, "dotted_name") else {
                        continue;
                    };
                    let alias_node = ts::child_of_type(*child, "identifier");
                    let imported = self.dotted_name(name_node);
                    let local = match alias_node {
                        Some(node) => self.text(node),
                        None => imported.clone(),
                    };
                    let ext_qname = qualify(&imported);
                    let alias = alias_node.map(|_| local.clone());
                    self.emit_import(
                        &local,
                        &ext_qname,
                        is_relative,
                        level,
                        alias.as_deref(),
                        false,
                    )?;
                }
                "wildcard_import" => {
                    let ext_qname = if source_module.is_empty() {
                        "*".to_string()
                    } else {
                        format!("{source_module}.*")
                    };
                    self.emit_import("*", &ext_qname, is_relative, level, None, true)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn emit_import(
        &mut self,
        local_name: &str,
        ext_qname: &str,
        is_relative: bool,
        level: usize,
        alias: Option<&str>,
        is_star: bool,
    ) -> PyResult<()> {
        let top_level = ext_qname.split('.').next().unwrap_or(ext_qname);
        let origin = if is_relative {
            "internal"
        } else {
            self.classifier.classify(top_level)
        };

        let import_qname = format!("{}.{local_name}", self.scope());
        let metadata = PyDict::new(self.py);
        metadata.set_item("alias", alias)?;
        metadata.set_item("is_relative", is_relative)?;
        metadata.set_item("level", level)?;
        metadata.set_item("original_name", ext_qname)?;
        metadata.set_item("is_star", is_star)?;
        metadata.set_item("origin", origin)?;

        let import_node = self.make_node(
            NodeKind::Import,
            &import_qname,
            local_name,
            None,
            metadata,
            None,
        )?;
        let import_id = import_node.get().id.clone();
        self.add_node_with_relation(import_node, RelationKind::Declares)?;

        // An internal import targets a MODULE node when one is already in the
        // graph; otherwise an EXTERNAL_SYMBOL, so the edge is never lost.
        let mut target_id = if origin == "internal" {
            self.find_module_node_id(ext_qname)
        } else {
            None
        };
        if target_id.is_none() {
            target_id = Some(self.get_or_create_external_symbol(ext_qname, origin)?);
        }
        let target_id = target_id.expect("the external symbol is created above");

        let file_id = self.file_node_id.to_string();
        self.push_relation(file_id, target_id.clone(), RelationKind::Imports)?;
        self.push_relation(import_id, target_id, RelationKind::ResolvesTo)?;
        Ok(())
    }

    /// A MODULE node's ID by exact match or the longest prefix.
    ///
    /// This is how `from mypackage.utils import Foo` lands on the
    /// `mypackage.utils` module even when Foo has no node of its own yet.
    fn find_module_node_id(&self, qname: &str) -> Option<String> {
        let parts: Vec<&str> = qname.split('.').collect();
        for length in (1..=parts.len()).rev() {
            let candidate = parts[..length].join(".");
            if let Some(id) = self.module_index.get(&candidate) {
                return Some(id.clone());
            }
        }
        None
    }

    // -- classes and functions --------------------------------------------

    fn handle_class(
        &mut self,
        node: TsNode<'_>,
        decorators: &[String],
        decorator_nodes: &[TsNode<'_>],
    ) -> PyResult<()> {
        let Some(name_node) = ts::child_of_type(node, "identifier") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.scope());

        let arg_list = ts::child_of_type(node, "argument_list");
        let mut bases: Vec<String> = Vec::new();
        if let Some(arg_list) = arg_list {
            for child in ts::children(arg_list) {
                let base = self.name_from_node(child);
                if !base.is_empty() {
                    bases.push(base);
                }
            }
        }

        let is_abstract = bases.iter().any(|b| b == "ABC" || b == "ABCMeta");
        let is_enum = bases.iter().any(|b| {
            let last = b.rsplit('.').next().unwrap_or(b);
            ENUM_BASES.contains(&last)
        });

        let metadata = PyDict::new(self.py);
        metadata.set_item("decorators", PyList::new(self.py, decorators)?)?;
        metadata.set_item("bases", PyList::new(self.py, &bases)?)?;
        metadata.set_item("is_abstract", is_abstract)?;
        metadata.set_item("is_enum", is_enum)?;

        let class_node = self.make_node(
            NodeKind::Class,
            &qname,
            &name,
            Some(node),
            metadata,
            Some(name_node),
        )?;
        let class_id = class_node.get().id.clone();
        self.add_node_with_relation(class_node, RelationKind::Declares)?;

        // Decorator arguments are ordinary values (@deco(handler)).
        self.scan_decorators(decorator_nodes, &class_id)?;

        // Bases are recorded as occurrences; the resolver creates
        // INHERITS_FROM.
        if let Some(arg_list) = arg_list {
            for child in ts::children(arg_list) {
                if matches!(child.kind(), "identifier" | "attribute")
                    && let Some(base_name_node) = ts::first_identifier(child)
                {
                    self.record_occurrence("base", base_name_node, &class_id);
                }
            }
        }

        self.push(qname, class_id, NodeKind::Class);
        if let Some(body) = ts::child_of_type(node, "block") {
            self.visit_children(body)?;
        }
        self.pop();
        Ok(())
    }

    fn handle_function(
        &mut self,
        node: TsNode<'_>,
        decorators: &[String],
        decorator_nodes: &[TsNode<'_>],
    ) -> PyResult<()> {
        let children = ts::children(node);
        let is_async = children.iter().any(|c| c.kind() == "async");
        let kind = if self.kind_stack.last() == Some(&NodeKind::Class) {
            NodeKind::Method
        } else {
            NodeKind::Function
        };

        let Some(name_node) = ts::child_of_type(node, "identifier") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.scope());

        // The return annotation: a `type` node, but not the very first child.
        let first_id = children.first().map(|c| c.id());
        let type_node = children
            .iter()
            .find(|c| c.kind() == "type" && Some(c.id()) != first_id)
            .copied();
        let return_annotation = type_node.map(|n| self.text(n));

        let metadata = PyDict::new(self.py);
        metadata.set_item("decorators", PyList::new(self.py, decorators)?)?;
        metadata.set_item("is_async", is_async)?;
        metadata.set_item("is_classmethod", decorators.iter().any(|d| d == "classmethod"))?;
        metadata.set_item("is_staticmethod", decorators.iter().any(|d| d == "staticmethod"))?;
        metadata.set_item("is_property", decorators.iter().any(|d| d == "property"))?;
        metadata.set_item("return_annotation", return_annotation)?;

        let func_node =
            self.make_node(kind, &qname, &name, Some(node), metadata, Some(name_node))?;
        let func_id = func_node.get().id.clone();
        self.add_node_with_relation(func_node, RelationKind::Declares)?;

        self.scan_decorators(decorator_nodes, &func_id)?;

        if let Some(type_node) = type_node {
            self.record_annotation(type_node, &func_id);
            self.scan_annotation_calls(type_node, &func_id)?;
        }

        self.push(qname.clone(), func_id.clone(), kind);

        if let Some(params) = ts::child_of_type(node, "parameters") {
            self.extract_parameters(params, &func_id, &qname)?;
        }
        // A single pass over the body: calls, reads, and nested definitions
        // without double counting.
        if let Some(body) = ts::child_of_type(node, "block") {
            self.walk_body(body, &func_id)?;
        }

        self.pop();
        Ok(())
    }

    fn extract_parameters(
        &mut self,
        params_node: TsNode<'_>,
        function_id: &str,
        function_qname: &str,
    ) -> PyResult<()> {
        for child in ts::children(params_node) {
            let mut has_default = false;
            let mut is_variadic = false;
            let mut ann_type_node = None;

            let id_node = match child.kind() {
                "identifier" => Some(child),
                "default_parameter" => {
                    has_default = true;
                    ts::child_of_type(child, "identifier")
                }
                "typed_parameter" => {
                    ann_type_node = ts::child_of_type(child, "type");
                    ts::child_of_type(child, "identifier")
                }
                "typed_default_parameter" => {
                    has_default = true;
                    ann_type_node = ts::child_of_type(child, "type");
                    ts::child_of_type(child, "identifier")
                }
                "list_splat_pattern" | "dictionary_splat_pattern" => {
                    is_variadic = true;
                    ts::child_of_type(child, "identifier")
                }
                _ => None,
            };

            let Some(id_node) = id_node else { continue };
            let param_name = self.text(id_node);
            if param_name.is_empty() {
                continue;
            }
            let annotation = ann_type_node.map(|n| self.text(n));
            let param_qname = format!("{function_qname}.{param_name}");

            let metadata = PyDict::new(self.py);
            metadata.set_item("is_self", param_name == "self")?;
            metadata.set_item("is_cls", param_name == "cls")?;
            metadata.set_item("annotation", annotation)?;
            metadata.set_item("has_default", has_default)?;
            metadata.set_item("is_variadic", is_variadic)?;

            let param_node = self.make_node(
                NodeKind::Parameter,
                &param_qname,
                &param_name,
                Some(child),
                metadata,
                Some(id_node),
            )?;
            let param_id = param_node.get().id.clone();
            self.safe_add_node(param_node);
            self.push_relation(
                function_id.to_string(),
                param_id.clone(),
                RelationKind::Declares,
            )?;

            if let Some(ann_type_node) = ann_type_node {
                self.record_annotation(ann_type_node, &param_id);
                // Calls inside an annotation (Depends(get_dep)) are value
                // uses, and they are enclosed by the function, not the
                // parameter.
                self.scan_annotation_calls(ann_type_node, function_id)?;
            }
        }
        Ok(())
    }

    // -- value scanning ---------------------------------------------------

    /// The one place an expression in value position turns into occurrences:
    /// every identifier yields exactly one `read`, every call exactly one
    /// `call`.
    ///
    /// A call's receiver (`obj` in `obj.m()`) is not recorded. Value
    /// expressions contain no nested definitions, so no guard against them is
    /// needed here.
    fn scan_value(&mut self, node: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        if node.kind() == "call" {
            if let Some(callee) = ts::child_of_types(node, &["identifier", "attribute"]) {
                let name_node = if callee.kind() == "attribute" {
                    ts::children(callee).last().copied().unwrap_or(callee)
                } else {
                    callee
                };
                self.record_occurrence("call", name_node, enclosing_id);
            }
            if let Some(arg_list) = ts::child_of_type(node, "argument_list") {
                for child in ts::children(arg_list) {
                    if matches!(child.kind(), "(" | ")" | ",") {
                        continue;
                    }
                    if child.kind() == "keyword_argument" {
                        // The value only (the last child) — a kwarg's name
                        // must not produce REFERENCES.
                        if let Some(value) = ts::children(child).last() {
                            self.scan_value(*value, enclosing_id)?;
                        }
                    } else {
                        self.scan_value(child, enclosing_id)?;
                    }
                }
            }
            return Ok(());
        }
        if node.kind() == "identifier" {
            self.record_occurrence("read", node, enclosing_id);
            return Ok(());
        }
        for child in ts::children(node) {
            self.scan_value(child, enclosing_id)?;
        }
        Ok(())
    }

    fn walk_body(&mut self, body: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        for child in ts::children(body) {
            self.walk_statement(child, enclosing_id)?;
        }
        Ok(())
    }

    /// Handling one statement (or clause) inside a body.
    ///
    /// Assignments and returns go to their own handlers, nested definitions
    /// to `visit`, blocks and block clauses (else/except/elif/finally)
    /// recursively, and everything else is a value expression.
    fn walk_statement(&mut self, node: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        if NESTED_DEF_TYPES.contains(&node.kind()) {
            return self.visit(node);
        }
        if node.kind() == "expression_statement" {
            for child in ts::children(node) {
                if child.kind() == "assignment" {
                    self.handle_assignment(child)?;
                } else {
                    self.scan_value(child, enclosing_id)?;
                }
            }
            return Ok(());
        }
        if node.kind() == "return_statement" {
            return self.visit_return_statement(node);
        }
        for child in ts::children(node) {
            if child.kind() == "block" {
                self.walk_body(child, enclosing_id)?;
            } else if ts::has_child_of_type(child, "block") {
                self.walk_statement(child, enclosing_id)?;
            } else {
                self.scan_value(child, enclosing_id)?;
            }
        }
        Ok(())
    }

    fn record_occurrence(&mut self, role: &str, name_node: TsNode<'_>, enclosing_id: &str) {
        let span = ts::span(name_node);
        self.occurrences.push(OccurrenceRef {
            role: role.to_string(),
            line: span.start_line,
            col: span.start_col,
            enclosing_id: enclosing_id.to_string(),
            span,
        });
    }

    /// Records an `annotation` for the leading identifier of a `type` node.
    fn record_annotation(&mut self, type_node: TsNode<'_>, enclosing_id: &str) {
        if let Some(ident) = ts::first_identifier(type_node) {
            self.record_occurrence("annotation", ident, enclosing_id);
        }
    }

    /// Decorator arguments: `@deco(handler)` yields a `call` on deco and a
    /// `read` on handler. A bare `@deco` has no call — nothing to record.
    fn scan_decorators(
        &mut self,
        decorator_nodes: &[TsNode<'_>],
        enclosing_id: &str,
    ) -> PyResult<()> {
        for dec in decorator_nodes {
            if let Some(call) = ts::child_of_type(*dec, "call") {
                self.scan_value(call, enclosing_id)?;
            }
        }
        Ok(())
    }

    /// Calls inside a type annotation: `Annotated[T, Depends(get_dep)]`.
    /// Plain type identifiers stay with `record_annotation`.
    fn scan_annotation_calls(
        &mut self,
        type_node: TsNode<'_>,
        enclosing_id: &str,
    ) -> PyResult<()> {
        let mut calls = Vec::new();
        ts::find_calls(type_node, &mut calls);
        for call in calls {
            self.scan_value(call, enclosing_id)?;
        }
        Ok(())
    }

    // -- assignments ------------------------------------------------------

    fn visit_return_statement(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let container = self.container();
        for child in ts::children(node) {
            if child.kind() != "return" {
                self.scan_value(child, &container)?;
            }
        }
        Ok(())
    }

    fn visit_expression_statement(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let container = self.container();
        for child in ts::children(node) {
            if child.kind() == "assignment" {
                self.handle_assignment(child)?;
            } else {
                self.scan_value(child, &container)?;
            }
        }
        Ok(())
    }

    /// Creates a VARIABLE, ATTRIBUTE, or TYPE_ALIAS from an assignment.
    ///
    /// `x: TypeAlias = v` → TYPE_ALIAS; `self.attr = v` or a class body →
    /// ATTRIBUTE; otherwise VARIABLE.
    fn handle_assignment(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let children = ts::children(node);
        let Some(lhs) = children.first().copied() else {
            return Ok(());
        };
        let annotation = ts::child_of_type(node, "type");
        let last = children.last().copied();
        let rhs = last.filter(|n| n.id() != lhs.id());

        // For `self.attr = v` the name is the LAST identifier, not the first
        // (the first would be `self`).
        let name_node = if lhs.kind() == "attribute" {
            ts::children(lhs)
                .into_iter()
                .rev()
                .find(|c| c.kind() == "identifier")
        } else {
            ts::first_identifier(lhs)
        };
        let Some(name_node) = name_node else {
            return Ok(());
        };
        let name = self.text(name_node);

        let is_alias = annotation.is_some_and(|a| self.text(a) == "TypeAlias");
        let in_class = self.kind_stack.last() == Some(&NodeKind::Class);
        let is_self_attr = lhs.kind() == "attribute"
            && ts::children(lhs)
                .first()
                .is_some_and(|c| self.text(*c) == "self");

        let kind = if is_alias {
            NodeKind::TypeAlias
        } else if in_class || is_self_attr {
            NodeKind::Attribute
        } else {
            NodeKind::Variable
        };

        let qname = format!("{}.{name}", self.scope());
        let metadata = PyDict::new(self.py);
        metadata.set_item("is_constant", is_upper(&name))?;

        let var_node = self.make_node(
            kind,
            &qname,
            &name,
            Some(node),
            metadata,
            Some(name_node),
        )?;
        self.add_node_with_relation(var_node, RelationKind::Declares)?;

        let container = self.container();
        self.record_occurrence("write", name_node, &container);

        match rhs {
            Some(rhs) if !is_alias => self.scan_value(rhs, &container)?,
            Some(rhs) => {
                if let Some(ident) = ts::first_identifier(rhs) {
                    self.record_occurrence("annotation", ident, &container);
                }
            }
            None => {}
        }
        Ok(())
    }

    // -- graph plumbing ---------------------------------------------------

    fn get_or_create_external_symbol(&mut self, qname: &str, origin: &str) -> PyResult<String> {
        let sym_id = node_id(self.project_name, qname, NodeKind::ExternalSymbol.as_str());
        if !self.graph.has_node(&sym_id) {
            let metadata = PyDict::new(self.py);
            metadata.set_item("origin", origin)?;
            let node = Node {
                id: sym_id.clone(),
                kind: NodeKind::ExternalSymbol,
                qualified_name: qname.to_string(),
                name: qname.rsplit('.').next().unwrap_or(qname).to_string(),
                file_path: None,
                span: None,
                metadata: metadata.unbind(),
            };
            self.graph
                .insert_node(sym_id.clone(), Py::new(self.py, node)?);
        }
        Ok(sym_id)
    }

    fn make_node(
        &self,
        kind: NodeKind,
        qualified_name: &str,
        name: &str,
        ts_node: Option<TsNode<'_>>,
        metadata: Bound<'a, PyDict>,
        name_node: Option<TsNode<'_>>,
    ) -> PyResult<Py<Node>> {
        // SpanIndex needs name_span to map a definition's position back to
        // the graph node.
        if let Some(name_node) = name_node {
            metadata.set_item("name_span", ts::span(name_node))?;
        }
        let node = Node {
            id: node_id(self.project_name, qualified_name, kind.as_str()),
            kind,
            qualified_name: qualified_name.to_string(),
            name: name.to_string(),
            file_path: Some(self.file_path.to_string()),
            span: ts_node.map(ts::span),
            metadata: metadata.unbind(),
        };
        Py::new(self.py, node)
    }

    fn add_node_with_relation(
        &mut self,
        node: Py<Node>,
        rel_kind: RelationKind,
    ) -> PyResult<()> {
        let node_id = node.get().id.clone();
        self.safe_add_node(node);
        self.push_relation(self.container(), node_id, rel_kind)
    }

    fn safe_add_node(&mut self, node: Py<Node>) {
        let id = node.get().id.clone();
        self.graph.insert_node(id, node);
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

    fn push(&mut self, qname: String, node_id: String, kind: NodeKind) {
        self.scope_stack.push(qname);
        self.container_stack.push(node_id);
        self.kind_stack.push(kind);
    }

    fn pop(&mut self) {
        self.scope_stack.pop();
        self.container_stack.pop();
        self.kind_stack.pop();
    }

    // -- names ------------------------------------------------------------

    fn dotted_name(&self, node: TsNode<'_>) -> String {
        ts::children(node)
            .into_iter()
            .filter(|c| c.kind() != ",")
            .map(|c| self.text(c))
            .collect()
    }

    fn name_from_node(&self, node: TsNode<'_>) -> String {
        match node.kind() {
            "identifier" => self.text(node),
            "attribute" => {
                let children = ts::children(node);
                let parent = children
                    .first()
                    .map(|c| self.name_from_node(*c))
                    .unwrap_or_default();
                let attr = children.last().map(|c| self.text(*c)).unwrap_or_default();
                if parent.is_empty() {
                    attr
                } else {
                    format!("{parent}.{attr}")
                }
            }
            _ => String::new(),
        }
    }

    fn decorator_name(&self, decorator_node: TsNode<'_>) -> String {
        for child in ts::children(decorator_node) {
            if matches!(child.kind(), "identifier" | "attribute" | "call") {
                let name = self.name_from_node(child);
                if !name.is_empty() {
                    return name;
                }
            }
        }
        String::new()
    }
}
