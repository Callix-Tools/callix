//! Walking TypeScript's CST and filling the graph.
//!
//! Laid out like the Python visitor: three stacks (name scope, parent, node
//! kind), structure is built as we go, and use-sites accumulate as
//! `OccurrenceRef`s for the resolution pass.

use std::collections::{BTreeMap, HashMap, HashSet};

use indexmap::IndexMap;

use crate::py::prelude::*;
use crate::py::types::{PyDict, PyList};
use crate::py::derive::{gen_stub_pyclass, gen_stub_pymethods};
use tree_sitter::Node as TsNode;

use crate::graph::Graph;
use crate::ids::node_id;
use crate::node::{Node, NodeKind};
use crate::occurrence::OccurrenceRef;
use crate::python::normalize_pkg_name;
use crate::relation::{Relation, RelationKind};
use crate::span::Span;
use crate::ts;

use super::module_resolver::{apply_alias, resolve_relative};

/// A method's name in a class body may sit in one of three slots.
const METHOD_NAME_TYPES: [&str; 3] = [
    "identifier",
    "property_identifier",
    "private_property_identifier",
];

/// Nested definitions are a scope boundary; the value scanner does not enter
/// them.
const NESTED_DEF_TYPES: [&str; 9] = [
    "function_declaration",
    "generator_function_declaration",
    "class_declaration",
    "abstract_class_declaration",
    "interface_declaration",
    "arrow_function",
    "function",
    "function_expression",
    "method_definition",
];

/// Statements that contain other statements.
const BLOCK_TYPES: [&str; 15] = [
    "statement_block",
    "if_statement",
    "else_clause",
    "for_statement",
    "for_in_statement",
    "while_statement",
    "do_statement",
    "try_statement",
    "catch_clause",
    "finally_clause",
    "switch_statement",
    "switch_body",
    "switch_case",
    "switch_default",
    "labeled_statement",
];

/// Determines an import's origin from precomputed name sets.
///
/// Unlike Python, the key is the package name derived from the specifier:
/// `@scope/pkg/sub` → `@scope/pkg`, `lodash/fp` → `lodash`.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core", frozen)]
#[derive(Default)]
pub struct TsImportClassifier {
    stdlib: HashSet<String>,
    third_party: HashSet<String>,
    internal: HashSet<String>,
}

#[gen_stub_pymethods]
#[pymethods]
impl TsImportClassifier {
    #[new]
    #[pyo3(signature = (stdlib = None, third_party = None, internal = None))]
    fn new(
        stdlib: Option<HashSet<String>>,
        third_party: Option<HashSet<String>>,
        internal: Option<HashSet<String>>,
    ) -> Self {
        Self {
            stdlib: stdlib.unwrap_or_default(),
            third_party: third_party.unwrap_or_default(),
            internal: internal.unwrap_or_default(),
        }
    }

    #[pyo3(name = "classify")]
    fn py_classify(&self, import_path: &str) -> &'static str {
        self.classify(import_path)
    }

    fn __repr__(&self) -> String {
        format!(
            "TsImportClassifier(stdlib={}, third_party={}, internal={})",
            self.stdlib.len(),
            self.third_party.len(),
            self.internal.len()
        )
    }
}

impl TsImportClassifier {
    pub fn from_sets(
        stdlib: HashSet<String>,
        third_party: HashSet<String>,
        internal: HashSet<String>,
    ) -> Self {
        Self { stdlib, third_party, internal }
    }

    pub fn classify(&self, import_path: &str) -> &'static str {
        let key = package_key(import_path);
        if self.stdlib.contains(&key) {
            return "stdlib";
        }
        if self.internal.contains(&key) {
            return "internal";
        }
        if self.third_party.contains(&key) {
            return "third_party";
        }
        "unknown"
    }
}

/// The package key from a module specifier.
fn package_key(import_path: &str) -> String {
    let pkg = if import_path.starts_with('@') {
        let parts: Vec<&str> = import_path.splitn(3, '/').collect();
        if parts.len() > 1 {
            parts[..2].join("/")
        } else {
            import_path.to_string()
        }
    } else {
        import_path.split('/').next().unwrap_or(import_path).to_string()
    };
    normalize_pkg_name(&pkg)
}

/// Strips the surrounding quotes from a string literal.
fn strip_quotes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > 1 {
        let first = chars[0];
        if (first == '\'' || first == '"' || first == '`') && *chars.last().unwrap() == first {
            return chars[1..chars.len() - 1].iter().collect();
        }
    }
    text.to_string()
}

/// A module path → the dotted name stored in the graph.
fn module_path_to_qname(module_path: &str, is_relative: bool, current_qname: &str) -> String {
    if is_relative {
        return resolve_relative(current_qname, module_path);
    }
    module_path.replace('/', ".")
}

/// A path → a safe identifier.
fn safe_name(path: &str) -> String {
    let mapped: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    mapped.trim_matches('_').to_string()
}

fn is_relative_path(path: &str) -> bool {
    path.starts_with("./") || path.starts_with("../") || path.starts_with('.')
}

pub struct TsVisitor<'a> {
    py: Python<'a>,
    graph: &'a mut Graph,
    source: &'a [u8],
    project_name: &'a str,
    file_path: &'a str,
    file_node_id: &'a str,
    source_root_name: String,
    classifier: &'a TsImportClassifier,
    /// The root's modules in full: name → id. Unlike the Python visitor, the
    /// map comes from outside and accumulates across files.
    modules: &'a IndexMap<String, String>,
    path_aliases: &'a BTreeMap<String, String>,
    scope_stack: Vec<String>,
    container_stack: Vec<String>,
    kind_stack: Vec<NodeKind>,
    occurrences: Vec<OccurrenceRef>,
    /// Overload signatures seen so far, by the qualified name of the function
    /// they belong to: the implementation that follows is declared at each.
    overloads: HashMap<String, Vec<Span>>,
}

impl<'a> TsVisitor<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        py: Python<'a>,
        graph: &'a mut Graph,
        source: &'a [u8],
        project_name: &'a str,
        file_path: &'a str,
        module_qname: &'a str,
        file_node_id: &'a str,
        source_root_name: String,
        classifier: &'a TsImportClassifier,
        modules: &'a IndexMap<String, String>,
        path_aliases: &'a BTreeMap<String, String>,
    ) -> Self {
        Self {
            py,
            graph,
            source,
            project_name,
            file_path,
            file_node_id,
            source_root_name,
            classifier,
            modules,
            path_aliases,
            scope_stack: vec![module_qname.to_string()],
            container_stack: vec![file_node_id.to_string()],
            kind_stack: vec![NodeKind::File],
            occurrences: Vec::new(),
            overloads: HashMap::new(),
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

    fn in_class(&self) -> bool {
        self.kind_stack.last() == Some(&NodeKind::Class)
    }

    // -- dispatch ---------------------------------------------------------

    pub fn visit(&mut self, node: TsNode<'_>) -> PyResult<()> {
        match node.kind() {
            "program" => self.visit_children(node),
            "expression_statement" => self.visit_expression_statement(node),
            "lexical_declaration" => self.handle_lexical_declaration(node),
            "export_statement" => self.visit_export_statement(node),
            "class_declaration" => self.handle_class(node, false, false),
            "abstract_class_declaration" => self.handle_class(node, true, false),
            "interface_declaration" => self.visit_interface(node),
            "type_alias_declaration" => self.visit_type_alias(node),
            "enum_declaration" => self.visit_enum(node),
            "public_field_definition" => self.visit_public_field(node),
            "property_signature" => self.visit_property_signature(node),
            "method_signature" | "abstract_method_signature" => self.visit_method_signature(node),
            "function_signature" => self.note_overload(node),
            "function_declaration" | "generator_function_declaration" | "method_definition" => {
                self.handle_function(node)
            }
            "import_statement" => self.visit_import_statement(node),
            "internal_module" | "module" => self.visit_namespace(node),
            _ => self.visit_children(node),
        }
    }

    /// `namespace N { export function f() {} }`: what is declared inside is `N.f`.
    fn visit_namespace(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let qname = format!("{}.{}", self.scope(), self.text(name));
        let container = self.container();
        self.push(qname, container, NodeKind::Module);
        let result = match node.child_by_field_name("body") {
            Some(body) => self.visit_children(body),
            None => Ok(()),
        };
        self.pop();
        result
    }

    fn visit_children(&mut self, node: TsNode<'_>) -> PyResult<()> {
        for child in ts::children(node) {
            self.visit(child)?;
        }
        Ok(())
    }

    /// A bare expression at file or class level.
    ///
    /// Inside a function body such statements are already handled by
    /// `walk_statement`, so occurrences must not be recorded twice — hence the
    /// check on the current scope's kind.
    fn visit_expression_statement(&mut self, node: TsNode<'_>) -> PyResult<()> {
        if matches!(
            self.kind_stack.last(),
            Some(NodeKind::Function) | Some(NodeKind::Method)
        ) {
            return Ok(());
        }
        let enclosing = self.container();
        let Some(expr) = ts::named_children(node).into_iter().next() else {
            return Ok(());
        };
        if matches!(expr.kind(), "internal_module" | "module") {
            return self.visit_namespace(expr);
        }
        if matches!(
            expr.kind(),
            "assignment_expression" | "augmented_assignment_expression"
        ) {
            if let Some(rhs) = ts::children(expr).last() {
                self.scan_value(*rhs, &enclosing)?;
            }
            return Ok(());
        }
        self.scan_value(expr, &enclosing)
    }

    // -- exports ----------------------------------------------------------

    fn visit_export_statement(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let children = ts::children(node);

        // `export { X } from 'mod'`: the string sits directly in the
        // export_statement, with no from_clause wrapper.
        let export_clause = children.iter().find(|c| c.kind() == "export_clause");
        let from_string = children.iter().find(|c| c.kind() == "string");
        if let (Some(_), Some(from_string)) = (export_clause, from_string) {
            let module_path = strip_quotes(&self.text(*from_string));
            if !module_path.is_empty() {
                self.emit_reexport(&module_path)?;
            }
            return Ok(());
        }

        for child in children {
            match child.kind() {
                "class_declaration"
                | "abstract_class_declaration"
                | "interface_declaration"
                | "function_declaration"
                | "generator_function_declaration"
                | "type_alias_declaration"
                | "enum_declaration" => return self.visit(child),
                "lexical_declaration" => return self.handle_lexical_declaration(child),
                _ => {}
            }
        }
        Ok(())
    }

    fn emit_reexport(&mut self, module_path: &str) -> PyResult<()> {
        let is_relative = is_relative_path(module_path);
        let local_name = format!(
            "__reexport_{}",
            module_path.replace(['/', '.'], "_")
        );
        let mut classify_path = module_path.to_string();
        if !is_relative && !self.path_aliases.is_empty() {
            let rewritten = apply_alias(&classify_path, self.path_aliases);
            if rewritten != classify_path {
                classify_path = self.strip_source_root_prefix(&rewritten);
            }
        }
        let ext_qname = module_path_to_qname(&classify_path, is_relative, self.scope());
        self.emit_import(&local_name, &ext_qname, is_relative, &classify_path, None, true)
    }

    /// Strips the source root's leading segment from a rewritten alias.
    ///
    /// The alias `@/client/v2` becomes `src/client/v2`; with a `.../src` root
    /// the leading `src/` has to go, or the name's top level would not match
    /// what was derived from file paths.
    fn strip_source_root_prefix(&self, path: &str) -> String {
        let prefix = format!("{}/", self.source_root_name);
        path.strip_prefix(&prefix).unwrap_or(path).to_string()
    }

    // -- classes and interfaces -------------------------------------------

    fn handle_class(
        &mut self,
        node: TsNode<'_>,
        is_abstract: bool,
        is_interface: bool,
    ) -> PyResult<()> {
        let children = ts::children(node);
        let Some(name_node) = children
            .iter()
            .find(|c| c.kind() == "type_identifier")
            .or_else(|| children.iter().find(|c| c.kind() == "identifier"))
            .copied()
        else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.scope());

        let heritage = if is_interface {
            ts::child_of_type(node, "extends_type_clause")
        } else {
            ts::child_of_type(node, "class_heritage")
                .and_then(|h| ts::child_of_type(h, "extends_clause"))
        };
        let (bases, base_nodes) = match heritage {
            Some(clause) => (self.heritage_bases(clause), heritage_base_nodes(clause)),
            None => (Vec::new(), Vec::new()),
        };

        let decorators = Self::decorators_of(node);
        let metadata = PyDict::new(self.py);
        metadata.set_item("decorators", PyList::new(self.py, self.decorator_names(&decorators))?)?;
        metadata.set_item("bases", PyList::new(self.py, &bases)?)?;
        metadata.set_item("is_abstract", is_abstract)?;
        if is_interface {
            metadata.set_item("is_interface", true)?;
        }

        let class_node =
            self.make_node(NodeKind::Class, &qname, &name, Some(node), metadata, Some(name_node))?;
        let class_id = class_node.get().id.clone();
        self.add_node_with_relation(class_node, RelationKind::Declares)?;
        self.scan_decorators(&decorators, &class_id)?;

        for base_node in base_nodes {
            self.record_occurrence("base", base_node, &class_id);
        }

        self.push(qname, class_id, NodeKind::Class);
        let body_kind = if is_interface { "interface_body" } else { "class_body" };
        if let Some(body) = ts::child_of_type(node, body_kind) {
            self.visit_children(body)?;
        }
        self.pop();
        Ok(())
    }

    fn visit_interface(&mut self, node: TsNode<'_>) -> PyResult<()> {
        self.handle_class(node, true, true)
    }

    /// An interface field `x: T;`.
    fn visit_property_signature(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(prop) = ts::child_of_type(node, "property_identifier") else {
            return Ok(());
        };
        let name = self.text(prop);
        let qname = format!("{}.{name}", self.scope());
        let attr = self.make_node(
            NodeKind::Attribute,
            &qname,
            &name,
            Some(node),
            PyDict::new(self.py),
            Some(prop),
        )?;
        let attr_id = attr.get().id.clone();
        self.add_node_with_relation(attr, RelationKind::Declares)?;
        if let Some(type_ann) = ts::child_of_type(node, "type_annotation") {
            self.record_annotation(type_ann, &attr_id);
        }
        Ok(())
    }

    /// An interface method signature `foo(): T;`.
    fn visit_method_signature(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = ts::child_of_types(node, &METHOD_NAME_TYPES) else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.scope());
        let method = self.make_node(
            NodeKind::Method,
            &qname,
            &name,
            Some(node),
            PyDict::new(self.py),
            Some(name_node),
        )?;
        let method_id = method.get().id.clone();
        self.add_node_with_relation(method, RelationKind::Declares)?;
        if let Some(type_ann) = ts::child_of_type(node, "type_annotation") {
            self.record_annotation(type_ann, &method_id);
        }
        if let Some(params) = ts::child_of_type(node, "formal_parameters") {
            self.extract_parameters(params, &method_id, &qname)?;
        }
        Ok(())
    }

    // -- type aliases and enums -------------------------------------------

    fn visit_type_alias(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = ts::child_of_type(node, "type_identifier") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.scope());
        let alias = self.make_node(
            NodeKind::TypeAlias,
            &qname,
            &name,
            Some(node),
            PyDict::new(self.py),
            Some(name_node),
        )?;
        let alias_id = alias.get().id.clone();
        self.add_node_with_relation(alias, RelationKind::Declares)?;

        // The aliased type is the first named child after `=`.
        let mut past_eq = false;
        for child in ts::children(node) {
            if !child.is_named() && child.kind() == "=" {
                past_eq = true;
                continue;
            }
            if past_eq && child.is_named() {
                self.record_annotation(child, &alias_id);
                break;
            }
        }
        Ok(())
    }

    fn visit_enum(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = ts::child_of_type(node, "identifier") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.scope());

        let metadata = PyDict::new(self.py);
        metadata.set_item("is_enum", true)?;
        let enum_node =
            self.make_node(NodeKind::Class, &qname, &name, Some(node), metadata, Some(name_node))?;
        let enum_id = enum_node.get().id.clone();
        self.add_node_with_relation(enum_node, RelationKind::Declares)?;

        self.push(qname.clone(), enum_id, NodeKind::Class);
        if let Some(body) = ts::child_of_type(node, "enum_body") {
            for child in ts::children(body) {
                let (member_node, name_node) = match child.kind() {
                    "property_identifier" => (child, child),
                    "enum_assignment" => {
                        let Some(prop) = ts::child_of_type(child, "property_identifier") else {
                            continue;
                        };
                        (child, prop)
                    }
                    _ => continue,
                };
                let member_name = self.text(name_node);
                let member_qname = format!("{qname}.{member_name}");
                let member = self.make_node(
                    NodeKind::Attribute,
                    &member_qname,
                    &member_name,
                    Some(member_node),
                    PyDict::new(self.py),
                    Some(name_node),
                )?;
                self.add_node_with_relation(member, RelationKind::Declares)?;
            }
        }
        self.pop();
        Ok(())
    }

    /// A class field `x: T = value;`.
    fn visit_public_field(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(prop) = ts::child_of_type(node, "property_identifier") else {
            return Ok(());
        };
        // A field holding an arrow function or a function expression is a method:
        // it is what `obj.f()` calls.
        if let Some(value) = ts::child_of_types(node, &["arrow_function", "function_expression", "function"]) {
            return self.handle_lexical_function(prop, value);
        }
        let name = self.text(prop);
        let qname = format!("{}.{name}", self.scope());
        let attr = self.make_node(
            NodeKind::Attribute,
            &qname,
            &name,
            Some(node),
            PyDict::new(self.py),
            Some(prop),
        )?;
        let attr_id = attr.get().id.clone();
        self.add_node_with_relation(attr, RelationKind::Declares)?;
        self.scan_decorators(&Self::decorators_of(node), &attr_id)?;

        // The use-site belongs to the class, not to the field itself — the
        // same as for variables in a lexical_declaration.
        let class_id = self.container();
        self.record_occurrence("write", prop, &class_id);

        if let Some(type_ann) = ts::child_of_type(node, "type_annotation") {
            self.record_annotation(type_ann, &attr_id);
        }
        let initializer = ts::children(node).into_iter().find(|c| {
            c.is_named()
                && !matches!(
                    c.kind(),
                    "property_identifier"
                        | "type_annotation"
                        | "accessibility_modifier"
                        | "abstract"
                        | "readonly"
                        | "static"
                        | "override"
                )
        });
        if let Some(initializer) = initializer {
            self.scan_value(initializer, &class_id)?;
        }
        Ok(())
    }

    // -- functions --------------------------------------------------------

    fn handle_function(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let children = ts::children(node);
        let is_async = children.iter().any(|c| c.kind() == "async");
        let kind = if self.in_class() { NodeKind::Method } else { NodeKind::Function };

        let Some(name_node) = ts::child_of_types(node, &METHOD_NAME_TYPES) else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.scope());

        let type_ann = ts::child_of_type(node, "type_annotation");
        let return_annotation = type_ann.map(|n| self.annotation_text(n));

        let decorators = Self::decorators_of(node);
        let metadata = PyDict::new(self.py);
        metadata.set_item("decorators", PyList::new(self.py, self.decorator_names(&decorators))?)?;
        metadata.set_item("is_async", is_async)?;
        metadata.set_item("return_annotation", return_annotation)?;
        if let Some(spans) = self.overloads.remove(&qname) {
            metadata.set_item("declared_at", PyList::new(self.py, spans)?)?;
        }

        let func = self.make_node(kind, &qname, &name, Some(node), metadata, Some(name_node))?;
        let func_id = func.get().id.clone();
        self.add_node_with_relation(func, RelationKind::Declares)?;
        self.scan_decorators(&decorators, &func_id)?;

        if let Some(type_ann) = type_ann {
            self.record_annotation(type_ann, &func_id);
        }

        self.push(qname.clone(), func_id.clone(), kind);
        if let Some(params) = ts::child_of_type(node, "formal_parameters") {
            self.extract_parameters(params, &func_id, &qname)?;
        }
        if let Some(body) = ts::child_of_type(node, "statement_block") {
            self.walk_body(body, &func_id)?;
        }
        self.pop();
        Ok(())
    }

    /// An overload signature `function f(x: number): void;`. It is no node: the
    /// implementation that follows is the function, declared at each signature,
    /// so that a call resolved to a signature still lands on it.
    fn note_overload(&mut self, node: TsNode<'_>) -> PyResult<()> {
        if let Some(name_node) = ts::child_of_type(node, "identifier") {
            let qname = format!("{}.{}", self.scope(), self.text(name_node));
            self.overloads.entry(qname).or_default().push(ts::span(name_node));
        }
        Ok(())
    }

    /// The annotation's text without the leading colon.
    fn annotation_text(&self, node: TsNode<'_>) -> String {
        self.text(node).trim_start_matches(':').trim().to_string()
    }

    // -- const / let ------------------------------------------------------

    fn handle_lexical_declaration(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let children = ts::children(node);
        let is_const = children.iter().any(|c| c.kind() == "const");

        for declarator in children {
            if declarator.kind() != "variable_declarator" {
                continue;
            }
            let Some(name_node) = ts::child_of_type(declarator, "identifier") else {
                continue;
            };
            let value = ts::child_of_types(
                declarator,
                &["arrow_function", "function", "function_expression"],
            );
            match value {
                Some(value) => self.handle_lexical_function(name_node, value)?,
                None => self.handle_lexical_variable(declarator, name_node, is_const)?,
            }
        }
        Ok(())
    }

    fn handle_lexical_function(
        &mut self,
        name_node: TsNode<'_>,
        value: TsNode<'_>,
    ) -> PyResult<()> {
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.scope());
        let is_async = ts::children(value).iter().any(|c| c.kind() == "async");
        let kind = if self.in_class() { NodeKind::Method } else { NodeKind::Function };

        // The return annotation hangs on the arrow function itself, not on
        // the declarator, whose children are `identifier = value`.
        let type_ann = ts::child_of_type(value, "type_annotation");
        let return_annotation = type_ann.map(|n| self.annotation_text(n));

        let metadata = PyDict::new(self.py);
        metadata.set_item("decorators", PyList::empty(self.py))?;
        metadata.set_item("is_async", is_async)?;
        metadata.set_item("return_annotation", return_annotation)?;
        // For a field holding a function the type checker answers with the
        // function's own position rather than the field's name; declare the
        // function at its first character as well.
        let start = ts::span(value);
        let at_start = Span {
            start_line: start.start_line,
            start_col: start.start_col,
            end_line: start.start_line,
            end_col: start.start_col + 1,
        };
        metadata.set_item("declared_at", PyList::new(self.py, vec![at_start])?)?;

        let func = self.make_node(kind, &qname, &name, Some(value), metadata, Some(name_node))?;
        let func_id = func.get().id.clone();
        self.add_node_with_relation(func, RelationKind::Declares)?;

        if let Some(type_ann) = type_ann {
            self.record_annotation(type_ann, &func_id);
        }

        self.push(qname.clone(), func_id.clone(), kind);
        if let Some(params) = ts::child_of_type(value, "formal_parameters") {
            self.extract_parameters(params, &func_id, &qname)?;
        }
        if let Some(body) = Self::function_body(value) {
            if body.kind() == "statement_block" {
                self.walk_body(body, &func_id)?;
            } else {
                self.scan_value(body, &func_id)?;
            }
        }
        self.pop();
        Ok(())
    }

    fn handle_lexical_variable(
        &mut self,
        declarator: TsNode<'_>,
        name_node: TsNode<'_>,
        is_const: bool,
    ) -> PyResult<()> {
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.scope());
        let kind = if self.in_class() { NodeKind::Attribute } else { NodeKind::Variable };

        let metadata = PyDict::new(self.py);
        metadata.set_item("is_constant", is_const)?;
        let var = self.make_node(
            kind,
            &qname,
            &name,
            Some(declarator),
            metadata,
            Some(name_node),
        )?;
        let var_id = var.get().id.clone();
        self.add_node_with_relation(var, RelationKind::Declares)?;

        let enclosing = self.container();
        self.record_occurrence("write", name_node, &enclosing);

        if let Some(type_ann) = ts::child_of_type(declarator, "type_annotation") {
            self.record_annotation(type_ann, &var_id);
        }

        // The initializer is found by filtering rather than by position: a
        // type annotation shifts it from the second named child to the third.
        let initializer = ts::children(declarator).into_iter().find(|c| {
            c.is_named() && c.id() != name_node.id() && c.kind() != "type_annotation"
        });
        if let Some(initializer) = initializer {
            self.record_aliases(initializer, &var_id, 0);
            if initializer.kind() == "object" {
                self.visit_object_literal(initializer, &qname, &var_id, &enclosing)?;
            } else {
                self.scan_value(initializer, &enclosing)?;
            }
        }
        Ok(())
    }

    /// `const o = { m() {}, f: () => {} }`: the members that are functions are
    /// functions `o.m` and `o.f`, declared by `o`, and what is called inside them
    /// is theirs. The other members are values.
    fn visit_object_literal(
        &mut self,
        object: TsNode<'_>,
        qname: &str,
        var_id: &str,
        enclosing: &str,
    ) -> PyResult<()> {
        for member in ts::children(object) {
            let function = match member.kind() {
                "method_definition" => Some((None, member)),
                "pair" => {
                    let named = ts::named_children(member);
                    match (named.first(), named.get(1)) {
                        (Some(key), Some(value))
                            if matches!(key.kind(), "property_identifier" | "identifier")
                                && matches!(
                                    value.kind(),
                                    "arrow_function" | "function_expression" | "function"
                                ) =>
                        {
                            Some((Some(*key), *value))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            match function {
                Some((None, method)) => {
                    self.push(qname.to_string(), var_id.to_string(), NodeKind::Variable);
                    let result = self.handle_function(method);
                    self.pop();
                    result?;
                }
                Some((Some(key), value)) => {
                    self.push(qname.to_string(), var_id.to_string(), NodeKind::Variable);
                    let result = self.handle_lexical_function(key, value);
                    self.pop();
                    result?;
                }
                None => self.scan_value(member, enclosing)?,
            }
        }
        Ok(())
    }

    // -- imports ----------------------------------------------------------

    fn visit_import_statement(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let module_path = match ts::child_of_type(node, "from_clause") {
            Some(from_clause) => ts::child_of_type(from_clause, "string")
                .map(|s| strip_quotes(&self.text(s)))
                .unwrap_or_default(),
            // A side-effect import: import 'mod'
            None => ts::child_of_type(node, "string")
                .map(|s| strip_quotes(&self.text(s)))
                .unwrap_or_default(),
        };
        if module_path.is_empty() {
            return Ok(());
        }

        let is_relative = is_relative_path(&module_path);
        // The node: scheme gets in the way of stdlib classification.
        let mut classify_path = module_path
            .strip_prefix("node:")
            .unwrap_or(&module_path)
            .to_string();
        // Aliases are rewritten before the name is computed, so @/client/v2 →
        // src/client/v2 → client.v2 and matches the internal modules.
        if !is_relative && !self.path_aliases.is_empty() {
            let rewritten = apply_alias(&classify_path, self.path_aliases);
            if rewritten != classify_path {
                classify_path = self.strip_source_root_prefix(&rewritten);
            }
        }
        let ext_qname = module_path_to_qname(&classify_path, is_relative, self.scope());

        let Some(import_clause) = ts::child_of_type(node, "import_clause") else {
            let local = format!("__sideeffect_{}", safe_name(&module_path));
            return self.emit_import(&local, &ext_qname, is_relative, &classify_path, None, true);
        };

        for child in ts::children(import_clause) {
            match child.kind() {
                // import X from 'mod'
                "identifier" => {
                    let local = self.text(child);
                    let qname = format!("{ext_qname}.default");
                    self.emit_import(
                        &local,
                        &qname,
                        is_relative,
                        &classify_path,
                        Some(&local.clone()),
                        false,
                    )?;
                }
                "named_imports" => {
                    self.process_named_imports(child, &ext_qname, is_relative, &classify_path)?;
                }
                // import * as NS from 'mod'
                "namespace_import" => {
                    if let Some(id_node) = ts::child_of_type(child, "identifier") {
                        let local = self.text(id_node);
                        self.emit_import(
                            &local,
                            &ext_qname,
                            is_relative,
                            &classify_path,
                            Some(&local.clone()),
                            true,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn process_named_imports(
        &mut self,
        named_imports: TsNode<'_>,
        ext_qname: &str,
        is_relative: bool,
        raw_path: &str,
    ) -> PyResult<()> {
        for spec in ts::children(named_imports) {
            if spec.kind() != "import_specifier" {
                continue;
            }
            let identifiers: Vec<TsNode<'_>> = ts::children(spec)
                .into_iter()
                .filter(|c| c.kind() == "identifier")
                .collect();
            match identifiers.len() {
                0 => continue,
                1 => {
                    let name = self.text(identifiers[0]);
                    let qname = format!("{ext_qname}.{name}");
                    self.emit_import(&name, &qname, is_relative, raw_path, None, false)?;
                }
                _ => {
                    // "original as alias"
                    let original = self.text(identifiers[0]);
                    let alias = self.text(identifiers[1]);
                    let qname = format!("{ext_qname}.{original}");
                    self.emit_import(
                        &alias,
                        &qname,
                        is_relative,
                        raw_path,
                        Some(&alias.clone()),
                        false,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn emit_import(
        &mut self,
        local_name: &str,
        ext_qname: &str,
        is_relative: bool,
        raw_path: &str,
        alias: Option<&str>,
        is_star: bool,
    ) -> PyResult<()> {
        let origin = if is_relative {
            "internal"
        } else {
            self.classifier
                .classify(if raw_path.is_empty() { ext_qname } else { raw_path })
        };

        let import_qname = format!("{}.{local_name}", self.scope());
        let metadata = PyDict::new(self.py);
        metadata.set_item("alias", alias)?;
        metadata.set_item("is_relative", is_relative)?;
        metadata.set_item("original_name", ext_qname)?;
        metadata.set_item("is_star", is_star)?;
        metadata.set_item("origin", origin)?;

        let import_node =
            self.make_node(NodeKind::Import, &import_qname, local_name, None, metadata, None)?;
        let import_id = import_node.get().id.clone();
        self.add_node_with_relation(import_node, RelationKind::Declares)?;

        let mut target_id = None;
        if origin == "internal" {
            let parts: Vec<&str> = ext_qname.split('.').collect();
            for length in (1..=parts.len()).rev() {
                let candidate = parts[..length].join(".");
                if let Some(id) = self.modules.get(&candidate) {
                    target_id = Some(id.clone());
                    break;
                }
            }
        }
        if target_id.is_none() {
            target_id = Some(self.get_or_create_external_symbol(ext_qname, origin)?);
        }
        let target_id = target_id.expect("the external symbol is created above");

        let file_id = self.file_node_id.to_string();
        self.push_relation(file_id, target_id.clone(), RelationKind::Imports)?;
        self.push_relation(import_id, target_id, RelationKind::ResolvesTo)
    }

    // -- parameters -------------------------------------------------------

    fn extract_parameters(
        &mut self,
        params_node: TsNode<'_>,
        function_id: &str,
        function_qname: &str,
    ) -> PyResult<()> {
        for child in ts::children(params_node) {
            let mut has_default = false;
            let mut is_variadic = false;
            let mut type_node = None;

            let name_node = match child.kind() {
                "identifier" => Some(child),
                "required_parameter" => {
                    let node = match ts::child_of_type(child, "rest_pattern") {
                        Some(rest) => {
                            is_variadic = true;
                            ts::child_of_type(rest, "identifier")
                        }
                        None => ts::child_of_types(child, &["identifier", "this"]),
                    };
                    type_node = ts::child_of_type(child, "type_annotation");
                    // A required parameter can carry a default value too
                    if ts::children(child).iter().any(|c| c.kind() == "=") {
                        has_default = true;
                    }
                    node
                }
                "optional_parameter" => {
                    type_node = ts::child_of_type(child, "type_annotation");
                    has_default = true;
                    ts::child_of_type(child, "identifier")
                }
                "rest_parameter" => {
                    is_variadic = true;
                    ts::child_of_type(child, "identifier")
                }
                "assignment_pattern" => {
                    has_default = true;
                    ts::child_of_type(child, "identifier")
                }
                _ => None,
            };

            // `@Body() dto`, `@Inject("x") service`: the function depends on them.
            self.scan_decorators(&Self::decorators_of(child), function_id)?;
            // A default is evaluated where the function is defined, and the
            // function depends on it: `opt = dep()` makes `dep` a dependency.
            if has_default && let Some(default) = Self::default_value(child) {
                self.scan_value(default, function_id)?;
            }

            let Some(name_node) = name_node else { continue };
            let param_name = self.text(name_node);
            if param_name.is_empty() || param_name == "this" {
                continue;
            }
            let annotation = type_node.map(|n| self.annotation_text(n));
            let param_qname = format!("{function_qname}.{param_name}");

            let metadata = PyDict::new(self.py);
            metadata.set_item("annotation", annotation)?;
            metadata.set_item("has_default", has_default)?;
            metadata.set_item("is_variadic", is_variadic)?;

            let param = self.make_node(
                NodeKind::Parameter,
                &param_qname,
                &param_name,
                Some(child),
                metadata,
                Some(name_node),
            )?;
            let param_id = param.get().id.clone();
            self.safe_add_node(param);
            self.push_relation(function_id.to_string(), param_id, RelationKind::Declares)?;

            if let Some(type_node) = type_node {
                self.record_annotation(type_node, function_id);
            }
        }
        Ok(())
    }

    // -- decorators -------------------------------------------------------

    /// The decorators written on `node`, in source order. A decorated class that
    /// is exported has them on the `export` wrapper instead.
    fn decorators_of<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
        let mut found: Vec<TsNode<'tree>> =
            ts::children(node).into_iter().filter(|c| c.kind() == "decorator").collect();
        // A class member's decorators are not inside it: the grammar puts them
        // in the class body, as the siblings just before the member.
        let mut previous = node.prev_sibling();
        while let Some(sibling) = previous {
            match sibling.kind() {
                "decorator" => found.push(sibling),
                "comment" => {}
                _ => break,
            }
            previous = sibling.prev_sibling();
        }
        if let Some(parent) = node.parent()
            && parent.kind() == "export_statement"
        {
            found.extend(ts::children(parent).into_iter().filter(|c| c.kind() == "decorator"));
        }
        found.sort_by_key(|d| d.start_byte());
        found
    }

    /// What each decorator is called: `@Get(":id")` is `Get`, `@a.B` is `a.B`.
    fn decorator_names(&self, decorators: &[TsNode<'_>]) -> Vec<String> {
        decorators
            .iter()
            .map(|decorator| {
                for child in ts::children(*decorator) {
                    let named = if child.kind() == "call_expression" {
                        ts::child_of_types(child, &["identifier", "member_expression"])
                    } else if matches!(child.kind(), "identifier" | "member_expression") {
                        Some(child)
                    } else {
                        None
                    };
                    if let Some(named) = named {
                        return self.name_from_node(named);
                    }
                }
                String::new()
            })
            .collect()
    }

    /// A decorator is a value its target depends on, in any of its forms:
    /// `@Guard` reads `Guard`, `@Get(":id")` calls `Get`, `@a.B(x)` reads `a`
    /// and `x` and calls `B`. The enclosing node is the decorated one.
    fn scan_decorators(
        &mut self,
        decorators: &[TsNode<'_>],
        enclosing_id: &str,
    ) -> PyResult<()> {
        // A decorator is called when the definition is evaluated, by the scope the
        // definition sits in; the decorated definition only depends on it. So the
        // call comes from the container and the reads stay with the definition.
        let container = self.container();
        for decorator in decorators {
            for child in ts::children(*decorator) {
                match child.kind() {
                    "@" => {}
                    "call_expression" => {
                        self.scan_call(child, enclosing_id, &container)?;
                        // The definition depends on what decorates it.
                        if let Some(name) = Self::callee_name(child) {
                            self.record_occurrence("read", name, enclosing_id);
                        }
                    }
                    "identifier" | "member_expression" => {
                        self.scan_value(child, enclosing_id)?;
                        let name = if child.kind() == "member_expression" {
                            ts::children(child).last().copied()
                        } else {
                            Some(child)
                        };
                        if let Some(name) = name {
                            self.record_occurrence("call", name, &container);
                        }
                    }
                    _ => self.scan_value(child, enclosing_id)?,
                }
            }
        }
        Ok(())
    }

    /// The value after the `=` of a parameter, in whichever shape it parses to.
    fn default_value<'tree>(parameter: TsNode<'tree>) -> Option<TsNode<'tree>> {
        let children = ts::children(parameter);
        let eq = children.iter().position(|c| c.kind() == "=")?;
        children.get(eq + 1).copied()
    }

    // -- use-sites --------------------------------------------------------

    /// The leading type identifier from an annotation.
    fn first_type_identifier<'tree>(&self, node: TsNode<'tree>) -> Option<TsNode<'tree>> {
        match node.kind() {
            "type_annotation" => ts::named_children(node)
                .into_iter()
                .next()
                .and_then(|c| self.first_type_identifier(c)),
            "type_identifier" | "identifier" => Some(node),
            // Built-in string/number/boolean — take the token itself.
            "predefined_type" => ts::children(node).into_iter().next(),
            // Base<T> — the base's name only.
            "generic_type" => ts::child_of_types(node, &["type_identifier", "identifier"]),
            // NS.Type — the trailing property.
            "member_expression" => ts::children(node).last().copied(),
            _ => ts::named_children(node)
                .into_iter()
                .find_map(|c| self.first_type_identifier(c)),
        }
    }

    fn record_annotation(&mut self, type_ann: TsNode<'_>, enclosing_id: &str) {
        if let Some(id_node) = self.first_type_identifier(type_ann) {
            self.record_occurrence("annotation", id_node, enclosing_id);
        }
    }

    fn record_occurrence(&mut self, role: &str, name_node: TsNode<'_>, enclosing_id: &str) {
        self.record_occurrence_with(role, name_node, enclosing_id, None);
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

    /// Scans a value expression, collecting `call` and `read`.
    fn scan_value(&mut self, node: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        // A closure written inline - a callback, an IIFE - is part of the function
        // that contains it: its calls are that function's. Named definitions are
        // scope boundaries and are visited as nodes of their own.
        if matches!(node.kind(), "arrow_function" | "function" | "function_expression") {
            return self.scan_closure(node, enclosing_id);
        }
        if NESTED_DEF_TYPES.contains(&node.kind()) {
            return Ok(());
        }

        match node.kind() {
            "new_expression" | "call_expression" => self.scan_call(node, enclosing_id, enclosing_id),
            "identifier" => {
                self.record_occurrence("read", node, enclosing_id);
                Ok(())
            }
            // An object literal { key: value } — only the value is scanned.
            "pair" => {
                let named = ts::named_children(node);
                if named.len() > 1 {
                    self.scan_value(named[1], enclosing_id)?;
                }
                Ok(())
            }
            // The shorthand { x } — the identifier is both key and value.
            "shorthand_property_identifier" => {
                self.record_occurrence("read", node, enclosing_id);
                Ok(())
            }
            _ => {
                let mut result = Ok(());
                ts::for_each_child(node, |child| {
                    result = self.scan_value(child, enclosing_id);
                    result.as_ref().map(|_| ()).map_err(|_| ())
                })
                .ok();
                result
            }
        }
    }

    /// A call or a `new`. `caller_id` makes it: the enclosing node, except for a
    /// decorator, which the scope the decorated definition sits in applies.
    fn scan_call(&mut self, node: TsNode<'_>, enclosing_id: &str, caller_id: &str) -> PyResult<()> {
        let mut call_site = None;
        if node.kind() == "new_expression" {
            // `new A(x)` instantiates A: a call, of the class.
            if let Some(constructor) = ts::named_children(node).first().copied() {
                if let Some(name) = Self::plain_name(constructor) {
                    if constructor.kind() == "member_expression" {
                        self.scan_receiver(constructor, name, enclosing_id)?;
                    }
                    self.record_occurrence("call", name, caller_id);
                    call_site = Some(ts::span(name));
                } else {
                    self.scan_value(constructor, enclosing_id)?;
                }
            }
        } else if let Some(callee) = ts::children(node).first().copied() {
            match callee.kind() {
                // `super(...)` calls the base class's constructor.
                "super" => {
                    self.record_occurrence("call", callee, caller_id);
                    call_site = Some(ts::span(callee));
                }
                "identifier" | "member_expression" => {
                    let name_node = if callee.kind() == "member_expression" {
                        // Everything before the final `.name` is the receiver:
                        // `build().find(1)` uses `build`, `a.b.c()` uses `a`.
                        let parts = ts::children(callee);
                        for part in parts.iter().take(parts.len().saturating_sub(1)) {
                            self.scan_value(*part, enclosing_id)?;
                        }
                        parts.last().copied().unwrap_or(callee)
                    } else {
                        callee
                    };
                    self.record_occurrence("call", name_node, caller_id);
                    call_site = Some(ts::span(name_node));
                }
                // `f()()`: a call of whatever `f` returns.
                "call_expression" => {
                    self.scan_call(callee, enclosing_id, caller_id)?;
                    if let Some(inner) = Self::callee_name(callee) {
                        self.record_occurrence_with("call", inner, caller_id, Some("result".into()));
                    }
                }
                // `table[key]()`: a call of whatever the container holds.
                "subscript_expression" => {
                    self.scan_value(callee, enclosing_id)?;
                    if let Some(base) = ts::children(callee).first().copied().and_then(Self::plain_name) {
                        self.record_occurrence("call", base, caller_id);
                    }
                }
                _ => self.scan_value(callee, enclosing_id)?,
            }
        }
        if let Some(args) = ts::child_of_type(node, "arguments") {
            let mut index = 0usize;
            for arg in ts::children(args) {
                if matches!(arg.kind(), "," | "(" | ")") {
                    continue;
                }
                match (call_site, Self::plain_name(arg)) {
                    (Some(site), Some(name)) => {
                        if arg.kind() == "member_expression" {
                            self.scan_receiver(arg, name, enclosing_id)?;
                        }
                        // A name handed to a call also says which parameter it
                        // is handed to: that is what lets a callback be followed
                        // to the parameter that finally calls it.
                        let detail = format!("arg:{}:{}:{index}", site.start_line, site.start_col);
                        self.record_occurrence_with("read", name, enclosing_id, Some(detail));
                    }
                    _ => self.scan_value(arg, enclosing_id)?,
                }
                index += 1;
            }
        }
        Ok(())
    }

    /// The parts of a member chain before its final name are read.
    fn scan_receiver(&mut self, chain: TsNode<'_>, name: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        for part in ts::children(chain) {
            if part.id() != name.id() && part.kind() != "." {
                self.scan_value(part, enclosing_id)?;
            }
        }
        Ok(())
    }

    /// The name a chain of identifiers and member accesses ends in - the position
    /// the resolver is asked about - when `node` is nothing but such a chain.
    fn plain_name<'t>(node: TsNode<'t>) -> Option<TsNode<'t>> {
        match node.kind() {
            "identifier" => Some(node),
            // `a.b`, `this.b`, `new A().b`, `f().b`: the position asked about is the
            // property; whatever is before it is read separately.
            "member_expression" => {
                ts::children(node).last().copied().filter(|l| l.kind() == "property_identifier")
            }
            _ => None,
        }
    }

    /// The name a call's callee ends in.
    fn callee_name<'t>(call: TsNode<'t>) -> Option<TsNode<'t>> {
        ts::children(call).first().copied().and_then(Self::plain_name)
    }

    /// What `value` hands to the variable it initialises: a name holds what it
    /// resolves to, a call what the callee returns, a container its elements.
    fn record_aliases(&mut self, value: TsNode<'_>, variable_id: &str, depth: usize) {
        if let Some(name) = Self::plain_name(value) {
            self.record_occurrence_with("alias", name, variable_id, None);
            return;
        }
        match value.kind() {
            "call_expression" => {
                if let Some(name) = Self::callee_name(value) {
                    self.record_occurrence_with("alias", name, variable_id, Some("result".into()));
                }
            }
            "subscript_expression" => {
                if let Some(base) = ts::children(value).first().copied().and_then(Self::plain_name) {
                    self.record_occurrence_with("alias", base, variable_id, None);
                }
            }
            "parenthesized_expression" | "as_expression" | "non_null_expression" | "satisfies_expression"
                if depth < 3 =>
            {
                if let Some(inner) = ts::named_children(value).first().copied() {
                    self.record_aliases(inner, variable_id, depth + 1);
                }
            }
            "object" | "array" if depth < 3 => {
                for element in ts::children(value) {
                    let element = if element.kind() == "pair" {
                        match ts::named_children(element).get(1).copied() {
                            Some(v) => v,
                            None => continue,
                        }
                    } else {
                        element
                    };
                    self.record_aliases(element, variable_id, depth + 1);
                }
            }
            "shorthand_property_identifier" => {
                self.record_occurrence_with("alias", value, variable_id, None);
            }
            _ => {}
        }
    }

    /// The body of a function-valued expression, which belongs to whoever
    /// contains the expression.
    fn scan_closure(&mut self, node: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        if let Some(params) = ts::child_of_type(node, "formal_parameters") {
            for param in ts::children(params) {
                if let Some(default) = Self::default_value(param) {
                    self.scan_value(default, enclosing_id)?;
                }
            }
        }
        if let Some(body) = Self::function_body(node) {
            if body.kind() == "statement_block" {
                self.walk_body(body, enclosing_id)?;
            } else {
                self.scan_value(body, enclosing_id)?;
            }
        }
        Ok(())
    }

    /// A function's body: the block, or the expression of `() => expr`.
    fn function_body<'t>(node: TsNode<'t>) -> Option<TsNode<'t>> {
        if let Some(block) = ts::child_of_type(node, "statement_block") {
            return Some(block);
        }
        let children = ts::children(node);
        let arrow = children.iter().position(|c| c.kind() == "=>")?;
        children.get(arrow + 1).copied()
    }

    fn walk_body(&mut self, body: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        for child in ts::children(body) {
            self.walk_statement(child, enclosing_id)?;
        }
        Ok(())
    }

    fn walk_statement(&mut self, node: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        let kind = node.kind();

        if matches!(
            kind,
            "function_declaration"
                | "generator_function_declaration"
                | "class_declaration"
                | "abstract_class_declaration"
                | "interface_declaration"
                | "export_statement"
        ) {
            return self.visit(node);
        }
        if kind == "lexical_declaration" {
            return self.handle_lexical_declaration(node);
        }
        if kind == "return_statement" {
            for child in ts::named_children(node) {
                // Calling the function yields what is returned.
                if let Some(name) = Self::plain_name(child) {
                    self.record_occurrence_with("alias", name, enclosing_id, Some("return".into()));
                }
                self.scan_value(child, enclosing_id)?;
            }
            return Ok(());
        }
        if kind == "expression_statement" {
            let Some(expr) = ts::named_children(node).into_iter().next() else {
                return Ok(());
            };
            if matches!(
                expr.kind(),
                "assignment_expression" | "augmented_assignment_expression"
            ) {
                if let Some(rhs) = ts::children(expr).last() {
                    self.scan_value(*rhs, enclosing_id)?;
                }
            } else {
                self.scan_value(expr, enclosing_id)?;
            }
            return Ok(());
        }
        if BLOCK_TYPES.contains(&kind) {
            for child in ts::children(node) {
                self.walk_statement(child, enclosing_id)?;
            }
            return Ok(());
        }
        if node.is_named() && !matches!(kind, "comment" | "formal_parameters") {
            self.scan_value(node, enclosing_id)?;
        }
        Ok(())
    }

    // -- names ------------------------------------------------------------

    fn name_from_node(&self, node: TsNode<'_>) -> String {
        match node.kind() {
            "identifier" => self.text(node),
            "member_expression" => {
                let children = ts::children(node);
                let parent = children
                    .first()
                    .map(|c| self.name_from_node(*c))
                    .unwrap_or_default();
                let property = children.last().map(|c| self.text(*c)).unwrap_or_default();
                if parent.is_empty() {
                    property
                } else {
                    format!("{parent}.{property}")
                }
            }
            _ => String::new(),
        }
    }

    fn heritage_bases(&self, heritage: TsNode<'_>) -> Vec<String> {
        let mut bases = Vec::new();
        for child in ts::children(heritage) {
            match child.kind() {
                "identifier" | "type_identifier" => bases.push(self.text(child)),
                "member_expression" => {
                    let name = self.name_from_node(child);
                    if !name.is_empty() {
                        bases.push(name);
                    }
                }
                "generic_type" => {
                    if let Some(name_node) =
                        ts::child_of_types(child, &["type_identifier", "identifier"])
                    {
                        bases.push(self.text(name_node));
                    }
                }
                _ => {}
            }
        }
        bases
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
        if let Some(name_node) = name_node {
            metadata.set_item("name_span", ts::span(name_node))?;
        }
        // What the author wrote about it: the comment block above.
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
            && let Some(doc) = crate::doc::leading_comment(definition, self.source)
        {
            metadata.set_item("doc", doc)?;
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

    fn add_node_with_relation(&mut self, node: Py<Node>, kind: RelationKind) -> PyResult<()> {
        let id = node.get().id.clone();
        self.safe_add_node(node);
        self.push_relation(self.container(), id, kind)
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
}

/// The name nodes for each base in an inheritance list.
fn heritage_base_nodes<'tree>(heritage: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut nodes = Vec::new();
    for child in ts::children(heritage) {
        match child.kind() {
            "identifier" | "type_identifier" => nodes.push(child),
            // NS.Base — the name is the trailing property.
            "member_expression" => {
                if let Some(last) = ts::children(child).last() {
                    nodes.push(*last);
                }
            }
            "generic_type" => {
                if let Some(name_node) =
                    ts::child_of_types(child, &["type_identifier", "identifier"])
                {
                    nodes.push(name_node);
                }
            }
            _ => {}
        }
    }
    nodes
}
