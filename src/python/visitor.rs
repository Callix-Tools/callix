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

/// Where the value of an expression goes: the variable that holds it, or an
/// element store (`d[k] = v`, `l.append(v)`) whose holder the resolver finds.
pub(crate) struct Sink {
    pub holder: String,
    pub store: Option<usize>,
}

impl Sink {
    fn holder(id: &str) -> Self {
        Sink { holder: id.to_owned(), store: None }
    }
}

/// The detail of an `alias` occurrence: the keys it sits under, the store it
/// belongs to, and what kind of value it is.
fn alias_detail(sink: &Sink, path: &[String], kind: Option<&str>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if !path.is_empty() {
        parts.push(format!("at:{}", path.join(";")));
    }
    if let Some(store) = sink.store {
        parts.push(format!("store:{store}"));
    }
    if let Some(kind) = kind {
        parts.push(kind.to_owned());
    }
    if parts.is_empty() { None } else { Some(parts.join("|")) }
}

/// What a key of a subscript is, as far as the source shows.
enum KeySpec<'t> {
    Literal(String),
    Wild,
    Slice(i64, i64),
    Name(TsNode<'t>),
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
    /// Numbers the element stores of this file (`d[k] = v`, `l.append(v)`).
    next_store: usize,
    /// The parameters of the functions being visited, innermost last: a call of
    /// one of them is a call of whatever it holds.
    param_scopes: Vec<(String, std::collections::HashSet<String>)>,
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
            next_store: 0,
            param_scopes: Vec::new(),
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
        self.param_scopes.push((qname.clone(), std::collections::HashSet::new()));

        if let Some(params) = ts::child_of_type(node, "parameters") {
            self.extract_parameters(params, &func_id, &qname)?;
        }
        // A single pass over the body: calls, reads, and nested definitions
        // without double counting.
        if let Some(body) = ts::child_of_type(node, "block") {
            self.walk_body(body, &func_id)?;
        }

        self.param_scopes.pop();
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
        self.param_scopes.push((qname.clone(), std::collections::HashSet::new()));
        if let Some(params) = ts::child_of_type(node, "lambda_parameters") {
            self.extract_parameters(params, &func_id, &qname)?;
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.scan_value(body, &func_id)?;
        }
        self.param_scopes.pop();
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
            if let Some((_, names)) = self.param_scopes.last_mut() {
                names.insert(param_name.clone());
            }

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
                // A default is what the parameter holds when the call passes nothing.
                self.record_aliases(*default, &Sink::holder(&param_id), &[], 0)?;
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
                } else if child.kind() == "call"
                    && let Some(name) = Self::callee_name(child)
                {
                    // `return f()`: calling this function yields what `f` returns.
                    self.record_occurrence_with("alias", name, &container, Some("return|result".into()));
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
            // `cb()` where `cb` is a parameter: whatever the type checker says of
            // it, the call is of what the parameter holds.
            if callee.kind() == "identifier" {
                let name = self.text(callee);
                let scope = self.param_scopes.iter().rev().find(|(_, names)| names.contains(&name)).map(|(q, _)| q.clone());
                if let Some(scope) = scope {
                    self.record_occurrence_with("paramcall", name_node, caller_id, Some(format!("p:{scope}.{name}")));
                }
            }
            // `map(xs, f)`, `sorted(xs, key=f)`: the library calls what it is given.
            for (argument, _) in self.callback_arguments(callee, node) {
                self.record_occurrence("callarg", argument, caller_id);
            }
            // `recv.m()`: what `recv` holds decides which class's `m` it is, when
            // the type checker cannot say. The receiver is recorded with the name
            // and the place of the call.
            if callee.kind() == "attribute"
                && let Some(receiver) = ts::children(callee).first().copied()
                && let Some(receiver_name) = Self::plain_name(receiver)
            {
                let at = ts::span(name_node);
                let detail = format!("m:{}@{}:{}", self.text(name_node), at.start_line, at.start_col);
                self.record_occurrence_with("recv", receiver_name, caller_id, Some(detail));
            }
            // `l.append(f)`, `d.update({...})`: what is stored in the container.
            if callee.kind() == "attribute" {
                self.record_method_store(node, callee, caller_id)?;
            }
        } else if let Some(callee) = ts::children(node).first().copied() {
            match callee.kind() {
                // `f()()`: a call of whatever `f` returns.
                "call" => {
                    self.scan_call(callee, enclosing_id, caller_id)?;
                    // How many calls are stacked below this one: `f()()()` is a call of
                    // what `f()()` returns, which is what `f()` returns.
                    let mut base = callee;
                    let mut depth = 1usize;
                    while let Some(function) = base.child_by_field_name("function")
                        && function.kind() == "call"
                    {
                        base = function;
                        depth += 1;
                    }
                    if let Some(inner) = Self::callee_name(base) {
                        // The arguments of this call are bound to what it calls, which is
                        // found only after resolution: they are keyed by the place of
                        // the argument list.
                        let site = ts::child_of_type(node, "argument_list").map(ts::span);
                        let detail = match site {
                            Some(at) => format!("result:{depth}:{}:{}", at.start_line, at.start_col),
                            None => format!("result:{depth}"),
                        };
                        self.record_occurrence_with("call", inner, caller_id, Some(detail));
                        call_site = site;
                    }
                }
                // `table["key"]()`: a call of whatever the container holds under it.
                "subscript" => {
                    self.scan_value(callee, enclosing_id)?;
                    if let Some((root, keys)) = Self::subscript_chain(callee) {
                        let parts = self.emit_keys(&keys, ts::span(root), caller_id);
                        self.record_occurrence_with("call", root, caller_id, Some(format!("keys:{}", parts.join(";"))));
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
    /// The arguments of a library function that calls them, among the plain
    /// names it is given, and whether what it yields are those calls' results.
    ///
    /// A closed list on purpose: `map` and `sorted` are known to call their
    /// function argument, and nothing else is guessed. A name the project defines
    /// itself is not told from the builtin here, which is the one place it can be
    /// wrong.
    fn callback_arguments<'t>(&self, callee: TsNode<'t>, call: TsNode<'t>) -> Vec<(TsNode<'t>, bool)> {
        // (every positional argument or only `key=`, results are yielded)
        let (positional, yields) = if callee.kind() == "identifier" {
            match self.text(callee).as_str() {
                "map" => (true, true),
                "filter" => (true, false),
                "sorted" | "min" | "max" => (false, false),
                _ => return Vec::new(),
            }
        } else {
            let parts = ts::children(callee);
            let object = parts.first().map(|p| self.text(*p));
            let name = parts.last().map(|p| self.text(*p)).unwrap_or_default();
            match (object.as_deref(), name.as_str()) {
                (Some("itertools"), "starmap") => (true, true),
                (Some("itertools"), "filterfalse" | "takewhile" | "dropwhile" | "accumulate") => (true, false),
                (Some("functools"), "reduce") => (true, false),
                (Some("itertools"), "groupby") => (false, false),
                (_, "sort") => (false, false),
                _ => return Vec::new(),
            }
        };
        let Some(arguments) = call.child_by_field_name("arguments") else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for argument in ts::children(arguments) {
            if argument.kind() == "keyword_argument" {
                if let (Some(name), Some(value)) = (argument.child_by_field_name("name"), argument.child_by_field_name("value"))
                    && self.text(name) == "key"
                    && let Some(plain) = Self::plain_name(value)
                {
                    found.push((plain, yields));
                }
            } else if positional && let Some(plain) = Self::plain_name(argument) {
                found.push((plain, yields));
            }
        }
        found
    }

    fn callee_name<'t>(call: TsNode<'t>) -> Option<TsNode<'t>> {
        ts::child_of_types(call, &["identifier", "attribute"]).and_then(Self::plain_name)
    }

    /// What `value` hands to the holder it is assigned to: a name holds what it
    /// resolves to, a call holds what the callee returns, a subscript holds what
    /// the container holds under that key, and a container literal holds its
    /// elements under their keys (`table["k"]`, `handlers[0]`).
    fn record_aliases(&mut self, value: TsNode<'_>, sink: &Sink, path: &[String], depth: usize) -> PyResult<()> {
        if let Some(name) = Self::plain_name(value) {
            self.record_occurrence_with("alias", name, &sink.holder, alias_detail(sink, path, None));
            return Ok(());
        }
        if let Some(lambda) = Self::unwrap_lambda(value) {
            let qname = self.visit_lambda(lambda)?;
            self.record_occurrence_with(
                "alias",
                lambda,
                &sink.holder,
                alias_detail(sink, path, Some(&format!("lambda:{qname}"))),
            );
            return Ok(());
        }
        match value.kind() {
            "call" => {
                if let Some(name) = Self::callee_name(value) {
                    self.record_occurrence_with("alias", name, &sink.holder, alias_detail(sink, path, Some("result")));
                }
                // `res = map(xs, f)`: what is iterated over are the results of `f`.
                if let Some(callee) = ts::child_of_types(value, &["identifier", "attribute"]) {
                    let mut deeper = path.to_vec();
                    deeper.push("*".to_owned());
                    for (argument, yields) in self.callback_arguments(callee, value) {
                        if yields {
                            self.record_occurrence_with("alias", argument, &sink.holder, alias_detail(sink, &deeper, Some("result")));
                        }
                    }
                }
            }
            "subscript" => {
                if let Some((root, keys)) = Self::subscript_chain(value) {
                    let parts = self.emit_keys(&keys, ts::span(root), &sink.holder);
                    let kind = format!("get:{}", parts.join(";"));
                    self.record_occurrence_with("alias", root, &sink.holder, alias_detail(sink, path, Some(&kind)));
                }
            }
            "parenthesized_expression" => {
                if let Some(inner) = ts::children(value).into_iter().find(|c| c.is_named()) {
                    self.record_aliases(inner, sink, path, depth)?;
                }
            }
            "dictionary" if depth < 4 => {
                for pair in ts::children(value) {
                    if pair.kind() != "pair" {
                        continue;
                    }
                    let (Some(key), Some(element)) = (pair.child_by_field_name("key"), pair.child_by_field_name("value"))
                    else {
                        continue;
                    };
                    let key = self.literal_key(key).unwrap_or_else(|| "*".to_owned());
                    let mut deeper = path.to_vec();
                    deeper.push(key);
                    self.record_aliases(element, sink, &deeper, depth + 1)?;
                }
            }
            "list" | "tuple" | "set" if depth < 4 => {
                let mut index = 0usize;
                for element in ts::children(value) {
                    if !element.is_named() || element.kind() == "comment" {
                        continue;
                    }
                    let mut deeper = path.to_vec();
                    deeper.push(if value.kind() == "set" { "*".to_owned() } else { format!("i{index}") });
                    index += 1;
                    if element.kind() == "list_splat" {
                        continue;
                    }
                    self.record_aliases(element, sink, &deeper, depth + 1)?;
                }
            }
            // `key = 1`: a literal is what an index can be read from.
            "integer" | "string" if depth == 0 && path.is_empty() => {
                if let Some(key) = self.literal_key(value) {
                    self.record_occurrence_with("alias", value, &sink.holder, alias_detail(sink, path, Some(&format!("lit:{key}"))));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The key a literal is, written the way the flow pass reads it: `i1`, `s'a'`.
    fn literal_key(&self, node: TsNode<'_>) -> Option<String> {
        match node.kind() {
            "integer" => {
                let text = self.text(node).replace('_', "");
                let value: i64 = text.parse().ok()?;
                Some(format!("i{value}"))
            }
            "string" => {
                let mut content = String::new();
                for child in ts::children(node) {
                    match child.kind() {
                        "string_start" => {
                            let start = self.text(child);
                            if !matches!(start.as_str(), "'" | "\"" | "'''" | "\"\"\"") {
                                return None;
                            }
                        }
                        "string_content" => content.push_str(&self.text(child)),
                        "string_end" => {}
                        _ => return None,
                    }
                }
                Some(format!("s{}", crate::flow::escape_key(&content)))
            }
            _ => None,
        }
    }

    /// `d["a"][k]`: the name at the root and the keys, innermost first.
    fn subscript_chain<'t>(node: TsNode<'t>) -> Option<(TsNode<'t>, Vec<TsNode<'t>>)> {
        let mut keys: Vec<TsNode<'t>> = Vec::new();
        let mut current = node;
        while current.kind() == "subscript" {
            keys.push(current.child_by_field_name("subscript")?);
            current = current.child_by_field_name("value")?;
        }
        if keys.is_empty() {
            return None;
        }
        let root = Self::plain_name(current)?;
        keys.reverse();
        Some((root, keys))
    }

    fn key_spec<'t>(&self, key: TsNode<'t>) -> KeySpec<'t> {
        if let Some(literal) = self.literal_key(key) {
            return KeySpec::Literal(literal);
        }
        match key.kind() {
            "slice" => {
                let mut segments: Vec<Option<TsNode<'t>>> = vec![None];
                for child in ts::children(key) {
                    if child.kind() == ":" {
                        segments.push(None);
                    } else if let Some(last) = segments.last_mut() {
                        *last = Some(child);
                    }
                }
                if segments.len() > 2 {
                    return KeySpec::Wild;
                }
                let bound = |segment: Option<TsNode<'t>>, default: i64| -> Option<i64> {
                    match segment {
                        None => Some(default),
                        Some(node) => self.literal_key(node)?.strip_prefix('i')?.parse().ok(),
                    }
                };
                match (bound(segments[0], 0), bound(segments.get(1).copied().flatten(), 1_000_000_000)) {
                    (Some(low), Some(high)) => KeySpec::Slice(low, high),
                    _ => KeySpec::Wild,
                }
            }
            "identifier" | "attribute" => Self::plain_name(key).map(KeySpec::Name).unwrap_or(KeySpec::Wild),
            _ => KeySpec::Wild,
        }
    }

    /// The keys of a subscript chain as the flow pass reads them. A key that is a
    /// name is recorded as an occurrence of its own - the resolver says what it
    /// is - and stands in the path as `#position`.
    fn emit_keys(&mut self, keys: &[TsNode<'_>], site: Span, enclosing: &str) -> Vec<String> {
        let mut parts: Vec<String> = Vec::new();
        for (position, key) in keys.iter().enumerate() {
            match self.key_spec(*key) {
                KeySpec::Literal(literal) => parts.push(literal),
                KeySpec::Wild => parts.push("*".to_owned()),
                KeySpec::Slice(low, high) => parts.push(format!("r{low}:{high}")),
                KeySpec::Name(name) => {
                    parts.push(format!("#{position}"));
                    self.record_occurrence_with(
                        "key",
                        name,
                        enclosing,
                        Some(format!("k:{}:{}:{position}", site.start_line, site.start_col)),
                    );
                }
            }
        }
        parts
    }

    /// `d[k] = v`: `v` is stored in `d` under `k`.
    fn store_into_container(&mut self, lhs: TsNode<'_>, rhs: Option<TsNode<'_>>) -> PyResult<()> {
        let container = self.container();
        if let Some((root, keys)) = Self::subscript_chain(lhs) {
            let store = self.next_store;
            self.next_store += 1;
            let parts = self.emit_keys(&keys, ts::span(root), &container);
            self.record_occurrence_with("store", root, &container, Some(format!("store:{store}|keys:{}", parts.join(";"))));
            if let Some(rhs) = rhs {
                let sink = Sink { holder: container.clone(), store: Some(store) };
                self.record_aliases(rhs, &sink, &[], 0)?;
            }
        }
        self.scan_value(lhs, &container)?;
        if let Some(rhs) = rhs {
            self.scan_value(rhs, &container)?;
        }
        Ok(())
    }

    /// `l.append(f)`, `d.update({...})`, `d.setdefault(k, f)`: elements stored
    /// through a method.
    fn record_method_store(&mut self, call: TsNode<'_>, callee: TsNode<'_>, caller_id: &str) -> PyResult<()> {
        let Some(method) = ts::children(callee).last().copied() else {
            return Ok(());
        };
        let name = self.text(method);
        if !matches!(name.as_str(), "append" | "add" | "insert" | "setdefault" | "update" | "extend") {
            return Ok(());
        }
        let Some(receiver) = ts::children(callee).first().copied() else {
            return Ok(());
        };
        let (root, mut key_nodes) = match Self::subscript_chain(receiver) {
            Some((root, keys)) => (root, keys),
            None => match Self::plain_name(receiver) {
                Some(root) => (root, Vec::new()),
                None => return Ok(()),
            },
        };
        let Some(arg_list) = ts::child_of_type(call, "argument_list") else {
            return Ok(());
        };
        let arguments: Vec<TsNode<'_>> = ts::children(arg_list)
            .into_iter()
            .filter(|a| a.is_named() && !matches!(a.kind(), "comment" | "keyword_argument"))
            .collect();
        let store = self.next_store;
        // What is stored, and under which extra keys.
        let (extra, elements): (Vec<String>, Vec<(TsNode<'_>, Vec<String>)>) = match (name.as_str(), arguments.as_slice()) {
            ("append" | "add", [value]) => (vec!["*".to_owned()], vec![(*value, Vec::new())]),
            ("insert", [_, value]) => (vec!["*".to_owned()], vec![(*value, Vec::new())]),
            ("update", [value]) if value.kind() == "dictionary" => (Vec::new(), vec![(*value, Vec::new())]),
            ("extend", [value]) if matches!(value.kind(), "list" | "tuple") => (
                Vec::new(),
                ts::children(*value)
                    .into_iter()
                    .filter(|e| e.is_named() && e.kind() != "comment")
                    .map(|e| (e, vec!["*".to_owned()]))
                    .collect(),
            ),
            ("setdefault", [key, value]) => {
                key_nodes.push(*key);
                (Vec::new(), vec![(*value, Vec::new())])
            }
            _ => return Ok(()),
        };
        self.next_store += 1;
        let mut parts = self.emit_keys(&key_nodes, ts::span(root), caller_id);
        parts.extend(extra);
        self.record_occurrence_with("store", root, caller_id, Some(format!("store:{store}|keys:{}", parts.join(";"))));
        let sink = Sink { holder: caller_id.to_owned(), store: Some(store) };
        for (element, path) in elements {
            self.record_aliases(element, &sink, &path, 0)?;
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
        // `f(1)`, `f("a")`: the parameter holds the literal, which is what an index
        // read through it can be.
        if let (Some(site), Some(slot)) = (call_site, slot.as_ref())
            && let Some(key) = self.literal_key(value)
        {
            let slot = match slot {
                Slot::Index(i) => i.to_string(),
                Slot::Keyword(k) => format!("={k}"),
            };
            let detail = format!("arg:{}:{}:{slot}|lit:{key}", site.start_line, site.start_col);
            self.record_occurrence_with("read", value, enclosing_id, Some(detail));
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
        for (index, dec) in decorator_nodes.iter().enumerate() {
            for child in ts::children(*dec) {
                match child.kind() {
                    "@" => {}
                    "call" => {
                        self.scan_call(child, enclosing_id, &container)?;
                        // The definition depends on what decorates it.
                        if let Some(name) = Self::callee_name(child) {
                            self.record_occurrence("read", name, enclosing_id);
                            // `@make(x)`: the decorator is what `make(x)` returns.
                            self.record_occurrence_with("decorate", name, enclosing_id, Some(format!("d:{index}:call")));
                        }
                    }
                    "identifier" | "attribute" => {
                        self.scan_value(child, enclosing_id)?;
                        if let Some(name) = Self::plain_name(child) {
                            self.record_occurrence("call", name, &container);
                            self.record_occurrence_with("decorate", name, enclosing_id, Some(format!("d:{index}:name")));
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
                } else if child.kind() == "call"
                    && let Some(name) = Self::callee_name(child)
                {
                    // `return f()`: calling this function yields what `f` returns.
                    self.record_occurrence_with("alias", name, &container, Some("return|result".into()));
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

        match lhs.kind() {
            // `d[k] = v` stores into `d`; it declares nothing.
            "subscript" => return self.store_into_container(lhs, rhs),
            "pattern_list" | "tuple_pattern" | "list_pattern" => return self.handle_unpacking(node, lhs, rhs),
            _ => {}
        }

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
        let (var_id, is_alias) = self.declare_target(node, lhs, name_node, annotation)?;
        let container = self.container();

        // `x = y`: the variable holds what `y` resolves to. The enclosing node of
        // an `alias` is the variable, so the flow pass knows who holds it.
        if !is_alias && let Some(value) = rhs {
            self.record_aliases(value, &Sink::holder(&var_id), &[], 0)?;
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

    /// Declares the variable, attribute or type alias an assignment creates, and
    /// records the write. Returns its id and whether it is a type alias.
    fn declare_target(
        &mut self,
        node: TsNode<'_>,
        lhs: TsNode<'_>,
        name_node: TsNode<'_>,
        annotation: Option<TsNode<'_>>,
    ) -> PyResult<(String, bool)> {
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
        // For `self.x = v` the type checker answers with the start of `self.x`, not
        // with the name `x`: declare the attribute there as well.
        if lhs.kind() == "attribute" && kind == NodeKind::Attribute {
            let start = ts::span(lhs);
            let at_start = crate::span::Span {
                start_line: start.start_line,
                start_col: start.start_col,
                end_line: start.start_line,
                end_col: start.start_col + 1,
            };
            metadata.set_item("declared_at", PyList::new(self.py, vec![at_start])?)?;
        }

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
        Ok((var_id, is_alias))
    }

    /// `a, *b, c = x, y, z, w`: each name is a variable holding what stands in its
    /// place, and the starred name holds the elements in between.
    fn handle_unpacking(&mut self, node: TsNode<'_>, lhs: TsNode<'_>, rhs: Option<TsNode<'_>>) -> PyResult<()> {
        let container = self.container();
        let mut targets: Vec<(Option<String>, bool)> = Vec::new();
        for target in ts::children(lhs).into_iter().filter(|c| c.is_named() && c.kind() != "comment") {
            let (name, starred) = match target.kind() {
                "identifier" => (Some(target), false),
                "list_splat_pattern" => (ts::child_of_type(target, "identifier"), true),
                _ => (None, false),
            };
            let id = match name {
                Some(name) => Some(self.declare_target(node, name, name, None)?.0),
                None => None,
            };
            targets.push((id, starred));
        }
        if let Some(rhs) = rhs {
            let elements: Option<Vec<TsNode<'_>>> = match rhs.kind() {
                "expression_list" | "tuple" | "list" | "pattern_list" => Some(
                    ts::children(rhs).into_iter().filter(|c| c.is_named() && c.kind() != "comment").collect(),
                ),
                _ => None,
            };
            let star = targets.iter().position(|(_, starred)| *starred);
            if let Some(elements) = elements {
                let (count, total) = (targets.len(), elements.len());
                for (index, (id, starred)) in targets.iter().enumerate() {
                    let Some(id) = id else {
                        continue;
                    };
                    if *starred {
                        let end = total.saturating_sub(count - index - 1);
                        for (position, element) in (index..end).enumerate() {
                            self.record_aliases(elements[element], &Sink::holder(id), &[format!("i{position}")], 0)?;
                        }
                    } else {
                        let element = match star {
                            Some(star) if index > star => total.checked_sub(count - index),
                            _ => Some(index),
                        };
                        if let Some(element) = element.filter(|e| *e < total) {
                            self.record_aliases(elements[element], &Sink::holder(id), &[], 0)?;
                        }
                    }
                }
            } else if star.is_none() && let Some(name) = Self::plain_name(rhs) {
                // `a, b = pair`: each holds the element of `pair` at its position.
                for (index, (id, _)) in targets.iter().enumerate() {
                    if let Some(id) = id {
                        self.record_occurrence_with("alias", name, id, Some(format!("get:i{index}")));
                    }
                }
            }
            self.scan_value(rhs, &container)?;
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
