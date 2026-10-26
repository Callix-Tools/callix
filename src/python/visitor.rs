//! Walking Python's CST and filling the graph.
//!
//! The state is three stacks, pushed and popped as scopes change:
//! `scope_stack` (the qualified-name prefix), `container_stack` (the parent's
//! id), and `kind_stack` (to tell METHOD from FUNCTION).
//!
//! The visitor creates no CALLS/INHERITS_FROM/REFERENCES/HAS_TYPE edges — it
//! collects `OccurrenceRef`s, which the resolution pass binds to definitions.

use std::collections::HashMap;

use crate::py::prelude::*;
use crate::py::types::{PyDict, PyList};
use tree_sitter::Node as TsNode;

use crate::graph::Graph;
use crate::ids::node_id;
use crate::node::{Node, NodeKind};
use crate::occurrence::{ImportClassifier, OccurrenceRef};
use crate::relation::{Relation, RelationKind};
use crate::span::Span;
use crate::ts;

use super::resolve_relative_import;

/// Which parameter of a call an argument is passed to.
enum Slot {
    Index(usize),
    Keyword(String),
}

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

/// A node and everything under it, depth-first.
fn descendants_of<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        out.push(current);
        stack.extend(ts::children(current));
    }
    out
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
    /// Every module of the root, including those whose file has not been
    /// visited yet: an import must not depend on which file comes first.
    project_modules: Option<&'a std::collections::HashSet<String>>,
    scope_stack: Vec<String>,
    container_stack: Vec<String>,
    kind_stack: Vec<NodeKind>,
    occurrences: Vec<OccurrenceRef>,
    /// How many lambdas each scope has had, for their names (`<lambda1>`).
    lambda_counts: HashMap<String, usize>,
    /// The qualified name given to the lambda that starts at a byte offset: a
    /// lambda reached by two paths of the walk is still one node.
    lambda_names: HashMap<usize, String>,
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
            project_modules: None,
            scope_stack: vec![module_qname.to_string()],
            container_stack: vec![file_node_id.to_string()],
            kind_stack: vec![NodeKind::File],
            occurrences: Vec::new(),
            lambda_counts: HashMap::new(),
            lambda_names: HashMap::new(),
        }
    }

    /// Gives the visitor the names of all the modules of the root.
    pub fn with_project_modules(mut self, modules: &'a std::collections::HashSet<String>) -> Self {
        self.project_modules = Some(modules);
        self
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
            // Compound statements at file or class level read their headers - the
            // condition, the iterable, the context manager, the raised class - as
            // surely as the same statements in a function do.
            "if_statement" | "for_statement" | "while_statement" | "with_statement"
            | "try_statement" | "raise_statement" | "assert_statement" | "match_statement"
            | "delete_statement" => {
                let container = self.container();
                self.walk_statement(node, &container)
            }
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
                    self.emit_import(&name, &name, false, 0, None, false, None)?;
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
                    self.emit_import(&local, &name, false, 0, alias.as_deref(), false, None)?;
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
                    self.emit_import(&imported, &ext_qname, is_relative, level, None, false, Some(*child))?;
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
                        Some(name_node),
                    )?;
                }
                "wildcard_import" => {
                    let ext_qname = if source_module.is_empty() {
                        "*".to_string()
                    } else {
                        format!("{source_module}.*")
                    };
                    self.emit_import("*", &ext_qname, is_relative, level, None, true, None)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_import(
        &mut self,
        local_name: &str,
        ext_qname: &str,
        is_relative: bool,
        level: usize,
        alias: Option<&str>,
        is_star: bool,
        name_node: Option<TsNode<'_>>,
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
        self.push_relation(import_id.clone(), target_id, RelationKind::ResolvesTo)?;

        // The name an internal `from m import name` brings in is a use-site:
        // the resolver follows it, through any re-export, to where `name` is
        // defined, and that definition is what the import resolves to. The
        // module edge above is the dependency; this one is the symbol.
        if origin == "internal"
            && !is_star
            && let Some(name_node) = name_node
            && let Some(last) = ts::children(name_node).into_iter().rev().find(|c| c.kind() == "identifier")
        {
            self.record_occurrence("import", last, &import_id);
        }
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
            // A module whose file comes later in the walk has no node yet, but
            // its id is a function of its name, and the node will be there.
            if self.project_modules.is_some_and(|known| known.contains(&candidate)) {
                return Some(node_id(self.project_name, &candidate, NodeKind::Module.as_str()));
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
                if matches!(child.kind(), "identifier" | "attribute" | "subscript")
                    && let Some(base_name_node) = ts::first_identifier(child)
                {
                    self.record_occurrence("base", base_name_node, &class_id);
                } else if child.kind() == "keyword_argument"
                    && let Some(value) = ts::children(child).last()
                {
                    // `class A(Base, metaclass=Meta)`: the value is used.
                    self.scan_value(*value, &class_id)?;
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

    /// A lambda is a function nobody named: a FUNCTION node called `<lambdaN>` in
    /// the scope it is written in, numbered in the order the scope meets them
    /// (`main.<lambda1>`, `main.func.<lambda2>`). Its parameters are PARAMETER
    /// nodes and its body belongs to it, so what it calls is its own.
    fn visit_lambda(&mut self, node: TsNode<'_>) -> PyResult<String> {
        if let Some(qname) = self.lambda_names.get(&node.start_byte()) {
            return Ok(qname.clone());
        }
        let scope = self.scope().to_string();
        let count = self.lambda_counts.entry(scope.clone()).or_insert(0);
        *count += 1;
        let name = format!("<lambda{count}>");
        let qname = format!("{scope}.{name}");
        self.lambda_names.insert(node.start_byte(), qname.clone());

        let metadata = PyDict::new(self.py);
        metadata.set_item("decorators", PyList::new(self.py, Vec::<String>::new())?)?;
        metadata.set_item("is_async", false)?;
        metadata.set_item("is_classmethod", false)?;
        metadata.set_item("is_staticmethod", false)?;
        metadata.set_item("is_property", false)?;
        metadata.set_item("return_annotation", Option::<String>::None)?;
        metadata.set_item("lambda", true)?;
        let keyword = ts::children(node).into_iter().find(|c| c.kind() == "lambda");
        let func_node =
            self.make_node(NodeKind::Function, &qname, &name, Some(node), metadata, keyword)?;
        let func_id = func_node.get().id.clone();
        self.add_node_with_relation(func_node, RelationKind::Declares)?;

        self.push(qname.clone(), func_id.clone(), NodeKind::Function);
        if let Some(params) = ts::child_of_type(node, "lambda_parameters") {
            self.extract_parameters(params, &func_id, &qname)?;
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.scan_value(body, &func_id)?;
        }
        self.pop();
        Ok(qname)
    }

    /// `(lambda: 1)` and `lambda: 1` are the same value.
    fn unwrap_lambda<'t>(node: TsNode<'t>) -> Option<TsNode<'t>> {
        let mut current = node;
        while current.kind() == "parenthesized_expression" {
            let inner: Vec<TsNode<'t>> = ts::children(current).into_iter().filter(|c| c.is_named()).collect();
            let [only] = inner.as_slice() else {
                return None;
            };
            current = *only;
        }
        (current.kind() == "lambda").then_some(current)
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

            // A default is evaluated where the function is defined, and the
            // function is what depends on it: `db=Depends(get_db)` makes
            // `get_db` a dependency of the handler.
            if has_default && let Some(default) = ts::children(child).last() {
                self.scan_value(*default, function_id)?;
            }

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
    /// A call's receiver (`obj` in `obj.m()`, and everything in a chain such as
    /// `session.query(Model).filter(cond()).all()`) is scanned like any other
    /// value: leaving it out dropped every name that is only ever used to call
    /// a method on. Value expressions contain no nested definitions, so no guard
    /// against them is needed here.
    fn scan_value(&mut self, node: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        if node.kind() == "lambda" {
            self.visit_lambda(node)?;
            return Ok(());
        }
        // A generator hands out what it yields: calling it, and iterating the
        // result, reaches the same values as a `return` would.
        if node.kind() == "yield" {
            let container = self.container();
            for child in ts::children(node) {
                if let Some(name) = Self::plain_name(child) {
                    self.record_occurrence_with("alias", name, &container, Some("return".into()));
                } else if let Some(lambda) = Self::unwrap_lambda(child) {
                    let qname = self.visit_lambda(lambda)?;
                    self.record_occurrence_with("alias", lambda, &container, Some(format!("return|lambda:{qname}")));
                }
            }
        }
        if node.kind() == "call" {
            return self.scan_call(node, enclosing_id, enclosing_id);
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

    /// A call. `caller_id` is who makes it - usually the enclosing node, but a
    /// decorator is applied by the scope the decorated definition sits in, not by
    /// the definition.
    fn scan_call(&mut self, node: TsNode<'_>, enclosing_id: &str, caller_id: &str) -> PyResult<()> {
        let mut call_site = None;
        if let Some(callee) = ts::child_of_types(node, &["identifier", "attribute"]) {
            let name_node = if callee.kind() == "attribute" {
                let parts = ts::children(callee);
                // Everything before the final `.name` is the receiver.
                for part in parts.iter().take(parts.len().saturating_sub(1)) {
                    self.scan_value(*part, enclosing_id)?;
                }
                parts.last().copied().unwrap_or(callee)
            } else {
                callee
            };
            self.record_occurrence("call", name_node, caller_id);
            call_site = Some(ts::span(name_node));
        } else if let Some(callee) = ts::children(node).first().copied() {
            match callee.kind() {
                // `f()()`: a call of whatever `f` returns.
                "call" => {
                    self.scan_call(callee, enclosing_id, caller_id)?;
                    if let Some(inner) = Self::callee_name(callee) {
                        self.record_occurrence_with("call", inner, caller_id, Some("result".into()));
                    }
                }
                // `table["key"]()`: a call of whatever the container holds.
                "subscript" => {
                    self.scan_value(callee, enclosing_id)?;
                    if let Some(base) = ts::children(callee).first().copied().and_then(Self::plain_name) {
                        self.record_occurrence("call", base, caller_id);
                    }
                }
                _ => self.scan_value(callee, enclosing_id)?,
            }
        }
        if let Some(arg_list) = ts::child_of_type(node, "argument_list") {
            let mut index = 0usize;
            for child in ts::children(arg_list) {
                if matches!(child.kind(), "(" | ")" | ",") {
                    continue;
                }
                if child.kind() == "keyword_argument" {
                    // The value only (the last child) — a kwarg's name
                    // must not produce REFERENCES.
                    if let Some(value) = ts::children(child).last().copied() {
                        let keyword = ts::children(child)
                            .first()
                            .filter(|n| n.kind() == "identifier")
                            .map(|n| self.text(*n));
                        self.scan_argument(value, enclosing_id, call_site, keyword.map(Slot::Keyword))?;
                    }
                } else {
                    self.scan_argument(child, enclosing_id, call_site, Some(Slot::Index(index)))?;
                    index += 1;
                }
            }
        }
        Ok(())
    }

    /// The identifier a call's callee ends in.
    fn callee_name<'t>(call: TsNode<'t>) -> Option<TsNode<'t>> {
        ts::child_of_types(call, &["identifier", "attribute"]).and_then(Self::plain_name)
    }

    /// What `value` hands to the variable it is assigned to: a name holds what it
    /// resolves to, a call holds what the callee returns, and a container holds
    /// whatever its elements are (so `table["k"]` can be followed to them).
    fn record_aliases(&mut self, value: TsNode<'_>, variable_id: &str, depth: usize) -> PyResult<()> {
        if let Some(name) = Self::plain_name(value) {
            self.record_occurrence_with("alias", name, variable_id, None);
            return Ok(());
        }
        if let Some(lambda) = Self::unwrap_lambda(value) {
            let qname = self.visit_lambda(lambda)?;
            self.record_occurrence_with("alias", lambda, variable_id, Some(format!("|lambda:{qname}")));
            return Ok(());
        }
        match value.kind() {
            "call" => {
                if let Some(name) = Self::callee_name(value) {
                    self.record_occurrence_with("alias", name, variable_id, Some("result".into()));
                }
            }
            "subscript" => {
                if let Some(base) = ts::children(value).first().copied().and_then(Self::plain_name) {
                    self.record_occurrence_with("alias", base, variable_id, None);
                }
            }
            "dictionary" | "list" | "tuple" | "set" if depth < 3 => {
                for element in ts::children(value) {
                    let element = if element.kind() == "pair" {
                        match ts::children(element).last().copied() {
                            Some(v) => v,
                            None => continue,
                        }
                    } else {
                        element
                    };
                    self.record_aliases(element, variable_id, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// One argument of a call. A plain name or attribute chain is a `read` that
    /// also says which call, and which parameter of it, it is passed to: that is
    /// what lets a callback be followed from the call site to the parameter that
    /// finally calls it.
    fn scan_argument(
        &mut self,
        value: TsNode<'_>,
        enclosing_id: &str,
        call_site: Option<Span>,
        slot: Option<Slot>,
    ) -> PyResult<()> {
        if let (Some(site), Some(slot), Some(name)) = (call_site, slot.as_ref(), Self::plain_name(value)) {
            if value.kind() == "attribute" {
                // The receiver of `obj.attr` is read as well.
                for part in ts::children(value) {
                    if part.id() != name.id() && part.kind() != "." {
                        self.scan_value(part, enclosing_id)?;
                    }
                }
            }
            let slot = match slot {
                Slot::Index(i) => i.to_string(),
                Slot::Keyword(k) => format!("={k}"),
            };
            let detail = format!("arg:{}:{}:{slot}", site.start_line, site.start_col);
            self.record_occurrence_with("read", name, enclosing_id, Some(detail));
            return Ok(());
        }
        // `g(lambda: 1)`: the parameter holds the lambda.
        if let (Some(site), Some(slot)) = (call_site, slot.as_ref())
            && let Some(lambda) = Self::unwrap_lambda(value)
        {
            let qname = self.visit_lambda(lambda)?;
            let slot = match slot {
                Slot::Index(i) => i.to_string(),
                Slot::Keyword(k) => format!("={k}"),
            };
            let detail = format!("arg:{}:{}:{slot}|lambda:{qname}", site.start_line, site.start_col);
            self.record_occurrence_with("read", lambda, enclosing_id, Some(detail));
            return Ok(());
        }
        // `g(f())`: the parameter holds whatever `f` returns.
        if let (Some(site), Some(slot)) = (call_site, slot)
            && value.kind() == "call"
            && let Some(function) = value.child_by_field_name("function")
            && let Some(name) = Self::plain_name(function)
        {
            let slot = match slot {
                Slot::Index(i) => i.to_string(),
                Slot::Keyword(k) => format!("={k}"),
            };
            let detail = format!("arg:{}:{}:{slot}:result", site.start_line, site.start_col);
            self.record_occurrence_with("read", name, enclosing_id, Some(detail));
        }
        self.scan_value(value, enclosing_id)
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
        if node.kind() == "raise_statement" {
            // `raise A` instantiates A as surely as `raise A()` does.
            if let Some(raised) = ts::children(node).into_iter().find(|c| c.kind() != "raise")
                && let Some(name) = Self::plain_name(raised)
            {
                self.record_occurrence("call", name, enclosing_id);
            }
            for child in ts::children(node) {
                if child.kind() != "raise" {
                    self.scan_value(child, enclosing_id)?;
                }
            }
            return Ok(());
        }
        if node.kind() == "for_statement"
            && let Some(iterable) = node.child_by_field_name("right")
        {
            let variable = self.declare_called_loop_variable(node)?;
            self.record_iteration(iterable, enclosing_id, variable);
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
        self.record_occurrence_with(role, name_node, enclosing_id, None);
    }

    /// `for x in it:` runs `it.__iter__()` and `it.__next__()`. Nothing in the
    /// source names them, so the iterable is recorded and the flow pass finds the
    /// class it holds: the name itself, or the callee of `Cls()`.
    fn record_iteration(&mut self, iterable: TsNode<'_>, enclosing_id: &str, variable: Option<String>) {
        let named = match iterable.kind() {
            "call" => iterable.child_by_field_name("function").and_then(Self::plain_name),
            _ => Self::plain_name(iterable),
        };
        if let Some(name) = named {
            let detail = variable.map(|qname| format!("var:{qname}"));
            self.record_occurrence_with("iterate", name, enclosing_id, detail);
        }
    }

    /// `for f in handlers: f()`. The loop variable is a node only when the body
    /// calls it - a node per loop counter would bury the graph - so that the call
    /// has something to land on and the flow pass something to follow. Returns its
    /// qualified name.
    fn declare_called_loop_variable(&mut self, node: TsNode<'_>) -> PyResult<Option<String>> {
        let (Some(target), Some(body)) = (node.child_by_field_name("left"), node.child_by_field_name("body")) else {
            return Ok(None);
        };
        if target.kind() != "identifier" {
            return Ok(None);
        }
        let name = self.text(target);
        let called = descendants_of(body).into_iter().any(|n| {
            n.kind() == "call"
                && n.child_by_field_name("function")
                    .is_some_and(|f| f.kind() == "identifier" && self.text(f) == name)
        });
        if !called {
            return Ok(None);
        }
        let qname = format!("{}.{name}", self.scope());
        let id = node_id(self.project_name, &qname, NodeKind::Variable.as_str());
        if !self.graph.has_node(&id) {
            let metadata = PyDict::new(self.py);
            metadata.set_item("is_constant", is_upper(&name))?;
            let variable = self.make_node(NodeKind::Variable, &qname, &name, Some(target), metadata, Some(target))?;
            self.add_node_with_relation(variable, RelationKind::Declares)?;
        }
        Ok(Some(qname))
    }

    fn record_occurrence_with(
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

    /// The identifier a name or attribute chain ends in - the position the
    /// resolver is asked about - when `node` is nothing but such a chain.
    fn plain_name<'t>(node: TsNode<'t>) -> Option<TsNode<'t>> {
        match node.kind() {
            "identifier" => Some(node),
            // `a.b`, `self.b`, `make().b`: the position asked about is the
            // attribute; whatever is before it is read separately.
            "attribute" => ts::children(node).into_iter().rev().find(|c| c.kind() == "identifier"),
            _ => None,
        }
    }

    /// Records an `annotation` for the leading identifier of a `type` node.
    fn record_annotation(&mut self, type_node: TsNode<'_>, enclosing_id: &str) {
        if let Some(ident) = ts::first_identifier(type_node) {
            self.record_occurrence("annotation", ident, enclosing_id);
        }
    }

    /// Decorators are values the decorated definition depends on, whatever form
    /// they take: `@deco(handler)` yields a `call` on deco and a `read` on
    /// handler, `@router.get("/x")` a `read` on `router` and a `call` on `get`,
    /// and a bare `@deco` a `read` on deco. The enclosing node is the decorated
    /// definition, so `decorated -> decorator` is how a reader finds the
    /// router behind a handler.
    fn scan_decorators(
        &mut self,
        decorator_nodes: &[TsNode<'_>],
        enclosing_id: &str,
    ) -> PyResult<()> {
        // A decorator is *called* when the definition is executed, which is by the
        // scope the definition sits in; the decorated definition only *depends* on
        // it. So the call edge comes from the container, and the reads (of the
        // router, of the arguments) stay with the definition.
        let container = self.container();
        for dec in decorator_nodes {
            for child in ts::children(*dec) {
                match child.kind() {
                    "@" => {}
                    "call" => {
                        self.scan_call(child, enclosing_id, &container)?;
                        // The definition depends on what decorates it.
                        if let Some(name) = Self::callee_name(child) {
                            self.record_occurrence("read", name, enclosing_id);
                        }
                    }
                    "identifier" | "attribute" => {
                        self.scan_value(child, enclosing_id)?;
                        if let Some(name) = Self::plain_name(child) {
                            self.record_occurrence("call", name, &container);
                        }
                    }
                    _ => self.scan_value(child, enclosing_id)?,
                }
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
                // Calling the function yields what is returned.
                if let Some(name) = Self::plain_name(child) {
                    self.record_occurrence_with("alias", name, &container, Some("return".into()));
                } else if let Some(lambda) = Self::unwrap_lambda(child) {
                    let qname = self.visit_lambda(lambda)?;
                    self.record_occurrence_with("alias", lambda, &container, Some(format!("return|lambda:{qname}")));
                }
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
        let var_id = var_node.get().id.clone();
        self.add_node_with_relation(var_node, RelationKind::Declares)?;

        let container = self.container();
        self.record_occurrence("write", name_node, &container);

        // `x = y`: the variable holds what `y` resolves to. The enclosing node of
        // an `alias` is the variable, so the flow pass knows who holds it.
        if !is_alias && let Some(value) = rhs {
            self.record_aliases(value, &var_id, 0)?;
        }

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
        // What the author wrote about it: the comment block above, and for a
        // Python function or class its docstring first.
        if let Some(definition) = ts_node
            && matches!(
                kind,
                NodeKind::Class
                    | NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::Variable
                    | NodeKind::Attribute
                    | NodeKind::TypeAlias
            )
        {
            let docstring: Option<String> = if matches!(
                kind,
                NodeKind::Class | NodeKind::Function | NodeKind::Method
            ) {
                crate::doc::python_docstring(definition, self.source)
            } else {
                None
            };
            if let Some(doc) =
                docstring.or_else(|| crate::doc::leading_comment(definition, self.source))
            {
                metadata.set_item("doc", doc)?;
            }
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
            // `@router.get("/x")` is named by what is called, `router.get`.
            let named = if child.kind() == "call" {
                ts::child_of_types(child, &["identifier", "attribute"])
            } else if matches!(child.kind(), "identifier" | "attribute") {
                Some(child)
            } else {
                None
            };
            if let Some(named) = named {
                let name = self.name_from_node(named);
                if !name.is_empty() {
                    return name;
                }
            }
        }
        String::new()
    }
}
