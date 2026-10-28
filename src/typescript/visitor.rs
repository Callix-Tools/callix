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
use super::type_uses::{TypeUse, alias_value, type_uses, value_chain};

/// A method's name in a class body may sit in one of three slots.
const METHOD_NAME_TYPES: [&str; 3] = [
    "identifier",
    "property_identifier",
    "private_property_identifier",
];

/// Nested definitions are a scope boundary; the value scanner does not enter
/// them. Inline function values (arrow functions, function expressions, and
/// methods or accessors of an object literal) are the exception: `scan_value`
/// handles them as closures of the containing scope before this check.
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

/// Library functions and methods that call a function they are given. The list is
/// closed on purpose: a function handed to anything else is a reference, since
/// whether the callee calls it cannot be known from the program. It applies only
/// where the callee is not a function of the project, whose business it is what
/// it does with an argument.
fn calls_its_arguments(name: &str) -> bool {
    matches!(
        name,
        // arrays, maps and sets
        "map" | "forEach" | "filter" | "reduce" | "reduceRight" | "some" | "every" | "find"
            | "findIndex" | "findLast" | "findLastIndex" | "flatMap" | "sort" | "toSorted" | "from"
            // promises
            | "then" | "catch" | "finally"
            // timers and events
            | "setTimeout" | "setInterval" | "setImmediate" | "queueMicrotask"
            | "requestAnimationFrame" | "addEventListener" | "on" | "once" | "addListener"
            | "subscribe"
            // hooks and test frameworks, which run what they are given
            | "useEffect" | "useLayoutEffect" | "useMemo" | "describe" | "it" | "test"
            | "beforeEach" | "afterEach" | "beforeAll" | "afterAll"
    )
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
    /// Start positions of every `read`/`write`/`call` already recorded, so a name
    /// is never recorded twice as a use.
    named_sites: HashSet<(u32, u32)>,
    /// Every module name that has a file or directory of its own (with all dotted
    /// prefixes). A `namespace` whose dotted name is in here makes no node: it
    /// would share an id with that MODULE.
    file_modules: Option<&'a HashSet<String>>,
    /// How many `ambient_declaration`s (`declare ...`) enclose the current node.
    ambient_depth: usize,
    /// One flag per namespace frame on the stack: is that namespace ambient?
    ambient_ns: Vec<bool>,
    /// Name spans of re-declarations (merged namespaces, merged interfaces,
    /// interface method overloads, same-named members of union arms), by node id.
    redeclared: IndexMap<String, Vec<Span>>,
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
            named_sites: HashSet::new(),
            file_modules: None,
            ambient_depth: 0,
            ambient_ns: Vec::new(),
            redeclared: IndexMap::new(),
        }
    }

    /// The module names that own a file or directory, so a `namespace` never
    /// shares an id with one of them.
    pub fn with_file_modules(mut self, modules: &'a HashSet<String>) -> Self {
        self.file_modules = Some(modules);
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
            "function_signature" => self.visit_function_signature(node),
            "ambient_declaration" => {
                self.ambient_depth += 1;
                let result = self.visit_children(node);
                self.ambient_depth -= 1;
                result
            }
            "function_declaration" | "generator_function_declaration" | "method_definition" => {
                self.handle_function(node)
            }
            "import_statement" => self.visit_import_statement(node),
            "internal_module" | "module" => self.visit_namespace(node),
            _ => self.visit_children(node),
        }
    }

    /// `namespace N { export function f() {} }`: N is a MODULE `<scope>.N`
    /// (metadata `is_namespace`, the extent of the declaration, DECLARED by its
    /// container) and what is declared inside is `N.f`. `namespace A.B {}` is two
    /// MODULEs, `A` and `A.B`. A string name (`declare module "x"`) has no stable
    /// qualified name: its body is walked under `<scope>."x"` and no node is made.
    fn visit_namespace(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let ambient = self.ambient_depth > 0;
        let mut frames = 0usize;
        if name.kind() == "string" {
            let qname = format!("{}.{}", self.scope(), self.text(name));
            let container = self.container();
            self.push(qname, container, NodeKind::Module);
            self.ambient_ns.push(false);
            frames = 1;
        } else {
            let mut segments = Vec::new();
            namespace_segments(name, &mut segments);
            for segment in segments {
                let segment_name = self.text(segment);
                let qname = format!("{}.{}", self.scope(), segment_name);
                let id = if self.is_file_module(&qname) {
                    node_id(self.project_name, &qname, NodeKind::Module.as_str())
                } else {
                    self.declare_namespace(node, segment, &qname, &segment_name)?
                };
                self.push(qname, id, NodeKind::Module);
                self.ambient_ns.push(ambient);
                frames += 1;
            }
        }
        let result = match node.child_by_field_name("body") {
            Some(body) => self.visit_children(body),
            None => Ok(()),
        };
        for _ in 0..frames {
            self.pop();
            self.ambient_ns.pop();
        }
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
        let Some(expr) = ts::named_children(node).into_iter().find(|c| c.kind() != "comment") else {
            return Ok(());
        };
        if matches!(expr.kind(), "internal_module" | "module") {
            return self.visit_namespace(expr);
        }
        if matches!(
            expr.kind(),
            "assignment_expression" | "augmented_assignment_expression"
        ) {
            if let Some(left) = expr.child_by_field_name("left") {
                self.scan_assignment_target(left, &enclosing)?;
            }
            if let Some(rhs) = expr.child_by_field_name("right") {
                self.scan_value(rhs, &enclosing)?;
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
                | "enum_declaration"
                | "ambient_declaration"
                | "function_signature" => return self.visit(child),
                "internal_module" | "module" => {
                    // `export declare namespace N {}` may parse with a bare `declare`
                    // keyword as a sibling instead of an ambient_declaration wrapper.
                    let ambient = ts::children(node).iter().any(|c| c.kind() == "declare");
                    if ambient {
                        self.ambient_depth += 1;
                    }
                    let result = self.visit(child);
                    if ambient {
                        self.ambient_depth -= 1;
                    }
                    return result;
                }
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

        for base_node in &base_nodes {
            self.record_occurrence("base", *base_node, &class_id);
        }
        self.record_type_parameters(node, &class_id);
        if let Some(clause) = heritage {
            self.record_heritage_uses(clause, &base_nodes, &class_id);
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

    /// An interface or type-literal field `x: T;` (also `"a-b": T` and `0: T`).
    fn visit_property_signature(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(prop) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let Some(name) = self.member_name(prop) else {
            return Ok(());
        };
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
        self.declare_or_redeclare(attr, prop)?;
        if let Some(type_ann) = ts::child_of_type(node, "type_annotation") {
            self.record_annotation(type_ann, &attr_id);
        }
        Ok(())
    }

    /// An interface method signature `foo(): T;`.
    fn visit_method_signature(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let Some(name) = self.member_name(name_node) else {
            return Ok(());
        };
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
        self.declare_or_redeclare(method, name_node)?;
        self.record_type_parameters(node, &method_id);
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

        // Every name of the aliased type, then the constraints and defaults of its parameters.
        if let Some(value) = alias_value(node) {
            self.walk_alias_type(value, &alias_id, &qname)?;
        }
        self.record_type_parameters(node, &alias_id);
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
        // The initializer is the `value` field: a comment is a named extra and a
        // decorator is a named child, so filtering by kind picks the wrong node.
        let initializer = node.child_by_field_name("value");
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
        self.record_type_parameters(node, &func_id);

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

    /// A function signature. Inside an ambient namespace (`declare namespace D {
    /// export function h(x: number): void; }`) it declares the FUNCTION, declared at
    /// every same-named signature of the block; anywhere else it is an overload note.
    fn visit_function_signature(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let ambient_ns = self.ambient_ns.last() == Some(&true)
            && self.kind_stack.last() == Some(&NodeKind::Module);
        if !ambient_ns {
            return self.note_overload(node);
        }
        let Some(name_node) = ts::child_of_type(node, "identifier") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = format!("{}.{name}", self.scope());
        let id = node_id(self.project_name, &qname, NodeKind::Function.as_str());
        if self.graph.has_node(&id) {
            return Ok(());
        }
        let mut holder = node.parent();
        if let Some(parent) = holder
            && parent.kind() == "export_statement"
        {
            holder = parent.parent();
        }
        if let Some(block) = holder {
            for sibling in ts::children(block) {
                let signature = match sibling.kind() {
                    "function_signature" => Some(sibling),
                    "export_statement" => ts::children(sibling)
                        .into_iter()
                        .find(|c| c.kind() == "function_signature"),
                    _ => None,
                };
                if let Some(signature) = signature
                    && let Some(other) = ts::child_of_type(signature, "identifier")
                    && self.text(other) == name
                {
                    self.overloads.entry(qname.clone()).or_default().push(ts::span(other));
                }
            }
        }
        self.handle_function(node)
    }

    /// The name of a member written as `x`, `#x`, `"a-b"` (unquoted) or `0`. A
    /// computed name (`[Symbol.iterator]`) has no stable name: no node.
    fn member_name(&self, name: TsNode<'_>) -> Option<String> {
        match name.kind() {
            "property_identifier" | "private_property_identifier" | "identifier" | "number" => {
                Some(self.text(name))
            }
            "string" => Some(strip_quotes(&self.text(name))),
            _ => None,
        }
    }

    /// Is `qname` the name of a module that owns a file or directory?
    fn is_file_module(&self, qname: &str) -> bool {
        self.modules.contains_key(qname)
            || self.file_modules.is_some_and(|set| set.contains(qname))
    }

    /// The MODULE of one segment of a namespace name. Returns its id; a second
    /// declaration of the same namespace adds a `declared_at` span, not a node.
    fn declare_namespace(
        &mut self,
        node: TsNode<'_>,
        segment: TsNode<'_>,
        qname: &str,
        name: &str,
    ) -> PyResult<String> {
        let metadata = PyDict::new(self.py);
        metadata.set_item("is_namespace", true)?;
        let module = self.make_node(NodeKind::Module, qname, name, Some(node), metadata, Some(segment))?;
        let id = module.get().id.clone();
        self.declare_or_redeclare(module, segment)?;
        Ok(id)
    }

    /// Adds `node` DECLARED by the current container, or - when a node with that id
    /// exists - records the new name span as one more place it is declared.
    fn declare_or_redeclare(&mut self, node: Py<Node>, name_node: TsNode<'_>) -> PyResult<()> {
        let id = node.get().id.clone();
        if self.graph.has_node(&id) {
            return self.redeclare(&id, name_node);
        }
        self.add_node_with_relation(node, RelationKind::Declares)
    }

    /// A re-declaration in the SAME file: the DECLARES edge again (folded by
    /// `dedupe_structural_relations`) and the name span in `declared_at`. In another
    /// file the first declaration wins and nothing is added, because `declared_at`
    /// spans are indexed under the node's own file.
    fn redeclare(&mut self, id: &str, name_node: TsNode<'_>) -> PyResult<()> {
        let same_file = self
            .graph
            .node_map()
            .get(id)
            .is_some_and(|n| n.get().file_path.as_deref() == Some(self.file_path));
        if !same_file {
            return Ok(());
        }
        self.push_relation(self.container(), id.to_string(), RelationKind::Declares)?;
        let span = ts::span(name_node);
        let known = self.redeclared.entry(id.to_string()).or_default();
        if !known.contains(&span) {
            known.push(span);
        }
        let spans = known.clone();
        if let Some(metadata) = self.graph.node_metadata(self.py, id) {
            metadata.bind(self.py).set_item("declared_at", PyList::new(self.py, spans)?)?;
        }
        Ok(())
    }

    /// The body of a type alias. Members of an object type - also in the arms of a
    /// top-level union, intersection or parenthesized type - are nodes `Alias.member`,
    /// like an interface's. Everything else (call/construct/index signatures,
    /// computed names, other types) is recorded as the alias's own annotation.
    fn walk_alias_type(&mut self, node: TsNode<'_>, alias_id: &str, alias_qname: &str) -> PyResult<()> {
        match node.kind() {
            "union_type" | "intersection_type" | "parenthesized_type" => {
                for child in ts::children(node) {
                    if child.is_named() {
                        self.walk_alias_type(child, alias_id, alias_qname)?;
                    }
                }
                Ok(())
            }
            "object_type" => {
                self.push(alias_qname.to_string(), alias_id.to_string(), NodeKind::TypeAlias);
                let mut result = Ok(());
                for child in ts::children(node) {
                    if !child.is_named() || child.kind() == "comment" {
                        continue;
                    }
                    let is_member = matches!(child.kind(), "property_signature" | "method_signature")
                        && child
                            .child_by_field_name("name")
                            .is_some_and(|n| self.member_name(n).is_some());
                    if is_member {
                        result = self.visit(child);
                        if result.is_err() {
                            break;
                        }
                    } else {
                        self.record_type_uses(child, alias_id, "annotation");
                    }
                }
                self.pop();
                result
            }
            _ => {
                self.record_type_uses(node, alias_id, "annotation");
                Ok(())
            }
        }
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
            // The name is a field: with a pattern for a name, the first `identifier`
            // child is the initializer.
            if let Some(pattern) = declarator
                .child_by_field_name("name")
                .filter(|n| matches!(n.kind(), "object_pattern" | "array_pattern"))
            {
                self.handle_destructuring(declarator, pattern, is_const)?;
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
        self.record_type_parameters(value, &func_id);

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

    /// `const { m, n: k } = o`, `const [a, b] = l`: each name is a variable that holds
    /// what the container holds under that key (or, for a rest element or a value
    /// that is not a name, whatever it holds at all). A nested pattern is not read.
    fn handle_destructuring(
        &mut self,
        declarator: TsNode<'_>,
        pattern: TsNode<'_>,
        is_const: bool,
    ) -> PyResult<()> {
        let enclosing = self.container();
        let initializer = declarator.child_by_field_name("value");
        // (the bound name, the key it is read with)
        let mut bound: Vec<(TsNode<'_>, Option<String>)> = Vec::new();
        let mut index = 0usize;
        for element in ts::children(pattern) {
            match (pattern.kind(), element.kind()) {
                ("object_pattern", "shorthand_property_identifier_pattern") => {
                    bound.push((element, self.literal_key(element)));
                }
                ("object_pattern", "object_assignment_pattern") => {
                    if let Some(left) = ts::named_children(element).first().copied() {
                        bound.push((left, self.literal_key(left)));
                    }
                }
                ("object_pattern", "pair_pattern") => {
                    if let (Some(key), Some(value)) = (element.child_by_field_name("key"), element.child_by_field_name("value")) {
                        let value = match value.kind() {
                            "assignment_pattern" => ts::named_children(value).first().copied(),
                            _ => Some(value),
                        };
                        if let Some(value) = value.filter(|v| v.kind() == "identifier") {
                            bound.push((value, self.literal_key(key)));
                        }
                    }
                }
                ("array_pattern", "identifier") => {
                    bound.push((element, Some(format!("i{index}"))));
                    index += 1;
                }
                ("array_pattern", "assignment_pattern") => {
                    if let Some(left) = ts::named_children(element).first().copied()
                        && left.kind() == "identifier"
                    {
                        bound.push((left, Some(format!("i{index}"))));
                    }
                    index += 1;
                }
                ("array_pattern", "rest_pattern") => {
                    if let Some(inner) = ts::named_children(element).first().copied().filter(|n| n.kind() == "identifier") {
                        bound.push((inner, None));
                    }
                    index += 1;
                }
                ("array_pattern", "," | "[" | "]" | "comment") => {}
                ("array_pattern", _) => index += 1,
                _ => {}
            }
        }
        for (name_node, key) in bound {
            let name = self.text(name_node);
            let qname = format!("{}.{name}", self.scope());
            let kind = if self.in_class() { NodeKind::Attribute } else { NodeKind::Variable };
            let metadata = PyDict::new(self.py);
            metadata.set_item("is_constant", is_const)?;
            let var = self.make_node(kind, &qname, &name, Some(declarator), metadata, Some(name_node))?;
            let var_id = var.get().id.clone();
            self.add_node_with_relation(var, RelationKind::Declares)?;
            self.record_occurrence("write", name_node, &enclosing);
            let Some(initializer) = initializer else {
                continue;
            };
            match (Self::plain_name(initializer), key.as_deref()) {
                (Some(source), Some(key)) => {
                    self.record_occurrence_with("alias", source, &var_id, Some(format!("get:{key}")));
                }
                // `const [a, b] = [f, g]`: the element at that key, written out.
                (None, Some(key)) if matches!(initializer.kind(), "array" | "object") => {
                    match self.element_at(initializer, key) {
                        Some(element) => self.record_aliases(element, &var_id, 0),
                        None => self.record_aliases(initializer, &var_id, 0),
                    }
                }
                _ => self.record_aliases(initializer, &var_id, 0),
            }
        }
        if let Some(initializer) = initializer {
            self.scan_value(initializer, &enclosing)?;
        }
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

        // The initializer is the `value` field: comments are named extras, so
        // "the first named child that is not the name" can be a comment.
        let initializer = declarator.child_by_field_name("value");
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
                    match (member.child_by_field_name("key"), member.child_by_field_name("value")) {
                        (Some(key), Some(value))
                            if matches!(key.kind(), "property_identifier" | "identifier")
                                && matches!(
                                    value.kind(),
                                    "arrow_function" | "function_expression" | "function"
                                ) =>
                        {
                            Some((Some(key), value))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            match function {
                Some((None, method)) => {
                    if let Some(name) = method.child_by_field_name("name")
                        && matches!(name.kind(), "property_identifier" | "string")
                    {
                        self.record_site("read", name, enclosing, Some("key"));
                    }
                    self.push(qname.to_string(), var_id.to_string(), NodeKind::Variable);
                    let result = self.handle_function(method);
                    self.pop();
                    result?;
                }
                Some((Some(key), value)) => {
                    if key.kind() == "property_identifier" {
                        self.record_site("read", key, enclosing, Some("key"));
                    }
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

            let Some(name_node) = name_node else {
                // `({ handler }: Props)`: a parameter of its own, so that the arguments
                // keep their places, and a variable for each name it unpacks.
                if let Some(pattern) = child
                    .child_by_field_name("pattern")
                    .filter(|p| matches!(p.kind(), "object_pattern"))
                {
                    self.declare_destructured_parameter(child, pattern, function_id, function_qname)?;
                }
                continue;
            };
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
            self.push_relation(function_id.to_string(), param_id.clone(), RelationKind::Declares)?;
            // `cb = fallback`: the parameter holds the default when nothing is passed.
            if has_default
                && let Some(default) = Self::default_value(child)
                && let Some(name) = Self::plain_name(default)
            {
                self.record_occurrence_with("alias", name, &param_id, None);
            }

            if let Some(type_node) = type_node {
                self.record_annotation(type_node, function_id);
            }
        }
        Ok(())
    }

    /// A parameter that is an object pattern: a PARAMETER for the whole, in its place
    /// among the parameters, and a VARIABLE for each name it unpacks, which holds what
    /// the argument holds under that name.
    fn declare_destructured_parameter(
        &mut self,
        parameter: TsNode<'_>,
        pattern: TsNode<'_>,
        function_id: &str,
        function_qname: &str,
    ) -> PyResult<()> {
        let declared = self
            .graph
            .relation_slice()
            .iter()
            .filter(|r| {
                let r = r.get();
                r.source_id == function_id && r.kind == RelationKind::Declares && self.graph.node_kind(&r.target_id) == Some(NodeKind::Parameter)
            })
            .count();
        let whole_name = format!("$param{declared}");
        let whole_qname = format!("{function_qname}.{whole_name}");
        let metadata = PyDict::new(self.py);
        metadata.set_item("has_default", false)?;
        metadata.set_item("is_variadic", false)?;
        let whole = self.make_node(NodeKind::Parameter, &whole_qname, &whole_name, Some(parameter), metadata, None)?;
        let whole_id = whole.get().id.clone();
        self.safe_add_node(whole);
        self.push_relation(function_id.to_string(), whole_id, RelationKind::Declares)?;
        for element in ts::children(pattern) {
            let (name, key) = match element.kind() {
                "shorthand_property_identifier_pattern" => (Some(element), self.literal_key(element)),
                "object_assignment_pattern" => {
                    let left = ts::named_children(element).first().copied();
                    (left, left.and_then(|l| self.literal_key(l)))
                }
                "pair_pattern" => {
                    let value = element.child_by_field_name("value").filter(|v| v.kind() == "identifier");
                    (value, element.child_by_field_name("key").and_then(|k| self.literal_key(k)))
                }
                _ => (None, None),
            };
            let (Some(name_node), Some(key)) = (name, key) else {
                continue;
            };
            let name = self.text(name_node);
            let qname = format!("{function_qname}.{name}");
            let metadata = PyDict::new(self.py);
            metadata.set_item("is_constant", false)?;
            let var = self.make_node(NodeKind::Variable, &qname, &name, Some(element), metadata, Some(name_node))?;
            let var_id = var.get().id.clone();
            self.safe_add_node(var);
            self.push_relation(function_id.to_string(), var_id.clone(), RelationKind::Declares)?;
            self.record_occurrence_with("alias", name_node, &var_id, Some(format!("get:{key}|from:{whole_qname}")));
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
        children[eq + 1..].iter().find(|c| c.kind() != "comment").copied()
    }

    // -- use-sites --------------------------------------------------------

    /// Records every name a type mentions. A type name gets `type_role`; a namespace prefix
    /// or a `typeof` operand is a `read`. Type parameters, mapped keys and `infer` names
    /// are not recorded (see `type_uses`).
    fn record_type_uses(&mut self, node: TsNode<'_>, enclosing_id: &str, type_role: &str) {
        let source = self.source;
        for (name, how) in type_uses(node, source) {
            let role = match how {
                TypeUse::Type => type_role,
                TypeUse::Value => "read",
            };
            self.record_occurrence(role, name, enclosing_id);
        }
    }

    /// A type annotation or aliased type in a declaration position: HAS_TYPE.
    fn record_annotation(&mut self, type_ann: TsNode<'_>, enclosing_id: &str) {
        self.record_type_uses(type_ann, enclosing_id, "annotation");
    }

    /// The constraints and defaults of `decl`'s type parameters (never their names).
    fn record_type_parameters(&mut self, decl: TsNode<'_>, owner_id: &str) {
        let Some(params) = ts::child_of_type(decl, "type_parameters") else {
            return;
        };
        for param in ts::children(params) {
            if param.kind() != "type_parameter" {
                continue;
            }
            for field in ["constraint", "value"] {
                if let Some(node) = param.child_by_field_name(field) {
                    self.record_type_uses(node, owner_id, "annotation");
                }
            }
        }
    }

    /// The names in an `extends` clause that are not the base itself: namespace prefixes
    /// (`ns.Base`, `a.Shape`) and type arguments. A class or module does not "have type"
    /// `Shape`, so these are `read`s from the class.
    fn record_heritage_uses(
        &mut self,
        clause: TsNode<'_>,
        base_nodes: &[TsNode<'_>],
        class_id: &str,
    ) {
        let source = self.source;
        let heads: Vec<usize> = base_nodes.iter().map(|n| n.start_byte()).collect();
        for child in ts::children(clause) {
            match child.kind() {
                "member_expression" => {
                    if let Some(object) = child.child_by_field_name("object") {
                        for name in value_chain(object, source) {
                            self.record_occurrence("read", name, class_id);
                        }
                    }
                }
                "nested_type_identifier" | "generic_type" | "type_arguments" => {
                    for (name, how) in type_uses(child, source) {
                        if how == TypeUse::Type && heads.contains(&name.start_byte()) {
                            continue;
                        }
                        self.record_occurrence("read", name, class_id);
                    }
                }
                _ => {}
            }
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
        if matches!(role, "read" | "write" | "call") {
            self.named_sites.insert((span.start_line, span.start_col));
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

    /// Records a use of a member name unless that position already has one.
    fn record_site(
        &mut self,
        role: &str,
        node: TsNode<'_>,
        enclosing_id: &str,
        detail: Option<&str>,
    ) {
        let span = ts::span(node);
        if self.named_sites.contains(&(span.start_line, span.start_col)) {
            return;
        }
        self.record_occurrence_with(role, node, enclosing_id, detail.map(str::to_string));
    }

    /// The left side of `a.b = v`: the property is written, the object is read.
    fn scan_assignment_target(&mut self, left: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        if left.kind() != "member_expression" {
            return Ok(());
        }
        if let Some(property) = left.child_by_field_name("property")
            && matches!(property.kind(), "property_identifier" | "private_property_identifier")
        {
            self.record_site("write", property, enclosing_id, Some("member"));
        }
        if let Some(object) = left.child_by_field_name("object") {
            self.scan_value(object, enclosing_id)?;
        }
        Ok(())
    }

    /// Scans a value expression, collecting `call` and `read`.
    fn scan_value(&mut self, node: TsNode<'_>, enclosing_id: &str) -> PyResult<()> {
        // A closure written inline - a callback, an IIFE - is part of the function
        // that contains it: its calls are that function's. Named definitions are
        // scope boundaries and are visited as nodes of their own.
        if matches!(node.kind(), "arrow_function" | "function" | "function_expression") {
            return self.scan_closure(node, enclosing_id);
        }
        // A method of an object literal: its name is the key of the member.
        if node.kind() == "method_definition"
            && node.parent().is_some_and(|p| p.kind() == "object")
            && let Some(name) = node.child_by_field_name("name")
            && matches!(name.kind(), "property_identifier" | "string")
        {
            self.record_site("read", name, enclosing_id, Some("key"));
        }
        // A method, getter or setter of an object literal is an inline closure of the
        // containing scope, as a callback is. A class-body method is a scope boundary
        // and keeps the early return below.
        if node.kind() == "method_definition" && node.parent().is_some_and(|p| p.kind() == "object") {
            return self.scan_closure(node, enclosing_id);
        }
        if NESTED_DEF_TYPES.contains(&node.kind()) {
            return Ok(());
        }

        match node.kind() {
            "new_expression" | "call_expression" => self.scan_call(node, enclosing_id, enclosing_id),
            // `s.kind`, `s?.kind`, `this.#p`: the property is read, then the object.
            "member_expression" => {
                if let Some(property) = node.child_by_field_name("property")
                    && matches!(property.kind(), "property_identifier" | "private_property_identifier")
                {
                    self.record_site("read", property, enclosing_id, None);
                }
                match node.child_by_field_name("object") {
                    Some(object) => self.scan_value(object, enclosing_id),
                    None => Ok(()),
                }
            }
            // `a.b = v` inside an expression (`() => this.x = 1`, `a = b.c = 1`).
            "assignment_expression" | "augmented_assignment_expression"
                if node
                    .child_by_field_name("left")
                    .is_some_and(|l| l.kind() == "member_expression") =>
            {
                if let Some(left) = node.child_by_field_name("left") {
                    self.scan_assignment_target(left, enclosing_id)?;
                }
                match node.child_by_field_name("right") {
                    Some(right) => self.scan_value(right, enclosing_id),
                    None => Ok(()),
                }
            }
            "identifier" => {
                self.record_occurrence("read", node, enclosing_id);
                Ok(())
            }
            // `x as T`, `x satisfies T`: scan the expression, the type is a set of reads.
            // Never fall to the default branch, which would walk the type as a value.
            "as_expression" | "satisfies_expression" => {
                let named = ts::named_children(node);
                if let Some(expression) = named.first() {
                    self.scan_value(*expression, enclosing_id)?;
                }
                // `x as const` has no type node: only the expression is named.
                if named.len() > 1
                    && let Some(ty) = named.last()
                {
                    self.record_type_uses(*ty, enclosing_id, "read");
                }
                Ok(())
            }
            // `<T>x`
            "type_assertion" => {
                for child in ts::named_children(node) {
                    if child.kind() == "type_arguments" {
                        self.record_type_uses(child, enclosing_id, "read");
                    } else {
                        self.scan_value(child, enclosing_id)?;
                    }
                }
                Ok(())
            }
            // `f<T>` without a call.
            "instantiation_expression" => {
                if let Some(function) = node.child_by_field_name("function") {
                    self.scan_value(function, enclosing_id)?;
                }
                if let Some(args) = node.child_by_field_name("type_arguments") {
                    self.record_type_uses(args, enclosing_id, "read");
                }
                Ok(())
            }
            // An object literal { key: value } — the key is a read of the member, then the value is scanned.
            "pair" => {
                if let Some(key) = node.child_by_field_name("key")
                    && matches!(key.kind(), "property_identifier" | "string")
                {
                    self.record_site("read", key, enclosing_id, Some("key"));
                }
                if let Some(value) = node.child_by_field_name("value") {
                    self.scan_value(value, enclosing_id)?;
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
        let mut callee_text: Option<String> = None;
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
                    callee_text = Some(self.text(name_node));
                    // `f.call(x)`, `f.apply(x, [])`: a call of `f`.
                    if callee.kind() == "member_expression"
                        && matches!(self.text(name_node).as_str(), "call" | "apply")
                        && let Some(object) = ts::children(callee).first().copied().and_then(Self::plain_name)
                    {
                        let at = ts::span(name_node);
                        let detail = format!("c:{}:{}", at.start_line, at.start_col);
                        self.record_occurrence_with("callarg", object, enclosing_id, Some(detail));
                    }
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
                        // `table["k"]()`, `fns[1]()`: a call made through that key.
                        let key = callee.child_by_field_name("index").and_then(|index| self.literal_key(index));
                        self.record_occurrence_with("call", base, caller_id, key.map(|k| format!("keys:{k}")));
                    }
                }
                _ => self.scan_value(callee, enclosing_id)?,
            }
        }
        // `f<Shape>(x)`, `new Box<Shape>(x)`: the type arguments are reads.
        if let Some(type_args) = node.child_by_field_name("type_arguments") {
            self.record_type_uses(type_args, enclosing_id, "read");
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
                        // `xs.map(f)`, `setTimeout(f)`: the library calls what it is given.
                        if callee_text.as_deref().is_some_and(calls_its_arguments) {
                            let detail = format!("c:{}:{}", site.start_line, site.start_col);
                            self.record_occurrence_with("callarg", name, enclosing_id, Some(detail));
                        }
                    }
                    (Some(site), None) if arg.kind() == "object" => {
                        // `run({ handler: cb })`: each name in the object is handed to the
                        // parameter under its key, for a parameter that unpacks them.
                        for pair in ts::children(arg) {
                            let (key, value) = match pair.kind() {
                                "pair" => (pair.child_by_field_name("key"), pair.child_by_field_name("value")),
                                "shorthand_property_identifier" => (Some(pair), Some(pair)),
                                _ => continue,
                            };
                            let (Some(key), Some(value)) = (key, value) else {
                                continue;
                            };
                            if let Some(written) = self.literal_key(key)
                                && let Some(name) = Self::plain_name(value).or(Some(value).filter(|v| v.kind() == "shorthand_property_identifier"))
                            {
                                let detail = format!("arg:{}:{}:{index}|at:{written}", site.start_line, site.start_col);
                                self.record_occurrence_with("alias", name, enclosing_id, Some(detail));
                            }
                        }
                        self.scan_value(arg, enclosing_id)?;
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
    /// resolves to, a call what the callee returns, a container its elements under
    /// the keys they were written with (`{ m: g }` holds `g` under `m`).
    fn record_aliases(&mut self, value: TsNode<'_>, variable_id: &str, depth: usize) {
        self.record_aliases_at(value, variable_id, depth, &[]);
    }

    /// The element a literal writes at `key`: the value of the pair `{ key: v }`, the
    /// `n`th element of `[a, b]` (when no spread comes before it).
    fn element_at<'t>(&self, literal: TsNode<'t>, key: &str) -> Option<TsNode<'t>> {
        let mut index = 0usize;
        for element in ts::children(literal) {
            match element.kind() {
                "pair" => {
                    let (written, value) = (element.child_by_field_name("key")?, element.child_by_field_name("value")?);
                    if self.literal_key(written).as_deref() == Some(key) {
                        return Some(value);
                    }
                }
                "shorthand_property_identifier" => {
                    if self.literal_key(element).as_deref() == Some(key) {
                        return Some(element);
                    }
                }
                "[" | "]" | "," | "{" | "}" | "comment" | "method_definition" => {}
                "spread_element" => return None,
                _ => {
                    if literal.kind() == "array" {
                        if key == format!("i{index}") {
                            return Some(element);
                        }
                        index += 1;
                    }
                }
            }
        }
        None
    }

    fn alias_detail(path: &[String], kind: Option<&str>) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if !path.is_empty() {
            parts.push(format!("at:{}", path.join(";")));
        }
        if let Some(kind) = kind {
            parts.push(kind.to_owned());
        }
        if parts.is_empty() { None } else { Some(parts.join("|")) }
    }

    /// The key a property name or an index is, as the flow pass reads it.
    fn literal_key(&self, node: TsNode<'_>) -> Option<String> {
        match node.kind() {
            "property_identifier" | "identifier" | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern" => {
                Some(format!("s{}", crate::flow::escape_key(&self.text(node))))
            }
            "string" => {
                let text = self.text(node);
                let inner = text.get(1..text.len().checked_sub(1)?)?;
                Some(format!("s{}", crate::flow::escape_key(inner)))
            }
            "number" => {
                let value: i64 = self.text(node).replace('_', "").parse().ok()?;
                Some(format!("i{value}"))
            }
            _ => None,
        }
    }

    fn record_aliases_at(&mut self, value: TsNode<'_>, variable_id: &str, depth: usize, path: &[String]) {
        if let Some(name) = Self::plain_name(value) {
            self.record_occurrence_with("alias", name, variable_id, Self::alias_detail(path, None));
            return;
        }
        match value.kind() {
            "call_expression" => {
                // `f.bind(x)` is a function that calls `f`: the same as holding `f`.
                let bound = ts::children(value).first().copied().filter(|c| c.kind() == "member_expression").and_then(|callee| {
                    let parts = ts::children(callee);
                    let property = parts.last().copied()?;
                    (self.text(property) == "bind").then(|| parts.first().copied().and_then(Self::plain_name)).flatten()
                });
                if let Some(target) = bound {
                    self.record_occurrence_with("alias", target, variable_id, Self::alias_detail(path, None));
                } else if let Some(name) = Self::callee_name(value) {
                    self.record_occurrence_with("alias", name, variable_id, Self::alias_detail(path, Some("result")));
                }
            }
            "subscript_expression" => {
                let parts = ts::children(value);
                if let Some(base) = parts.first().copied().and_then(Self::plain_name) {
                    // `o["m"]`, `l[0]`: what the container holds under that key.
                    let key = value
                        .child_by_field_name("index")
                        .and_then(|index| self.literal_key(index));
                    let kind = key.map(|key| format!("get:{key}"));
                    self.record_occurrence_with("alias", base, variable_id, Self::alias_detail(path, kind.as_deref()));
                }
            }
            "parenthesized_expression" | "as_expression" | "non_null_expression" | "satisfies_expression"
                if depth < 3 =>
            {
                if let Some(inner) = ts::named_children(value).first().copied() {
                    self.record_aliases_at(inner, variable_id, depth + 1, path);
                }
            }
            "object" if depth < 3 => {
                for element in ts::children(value) {
                    match element.kind() {
                        "pair" => {
                            let (Some(key), Some(inner)) = (element.child_by_field_name("key"), element.child_by_field_name("value")) else {
                                continue;
                            };
                            let mut deeper = path.to_vec();
                            deeper.push(self.literal_key(key).unwrap_or_else(|| "*".to_owned()));
                            self.record_aliases_at(inner, variable_id, depth + 1, &deeper);
                        }
                        "shorthand_property_identifier" => {
                            let mut deeper = path.to_vec();
                            deeper.push(self.literal_key(element).unwrap_or_else(|| "*".to_owned()));
                            self.record_occurrence_with("alias", element, variable_id, Self::alias_detail(&deeper, None));
                        }
                        "method_definition" | "," | "{" | "}" | "comment" => {}
                        _ => {}
                    }
                }
            }
            "array" if depth < 3 => {
                let mut index = 0usize;
                for element in ts::children(value) {
                    if matches!(element.kind(), "[" | "]" | "," | "comment") {
                        continue;
                    }
                    let mut deeper = path.to_vec();
                    deeper.push(if element.kind() == "spread_element" { "*".to_owned() } else { format!("i{index}") });
                    index += 1;
                    if element.kind() == "spread_element" {
                        continue;
                    }
                    self.record_aliases_at(element, variable_id, depth + 1, &deeper);
                }
            }
            "shorthand_property_identifier" => {
                self.record_occurrence_with("alias", value, variable_id, Self::alias_detail(path, None));
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
        node.child_by_field_name("body")
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
            let Some(expr) = ts::named_children(node).into_iter().find(|c| c.kind() != "comment") else {
                return Ok(());
            };
            if matches!(
                expr.kind(),
                "assignment_expression" | "augmented_assignment_expression"
            ) {
                if let Some(rhs) = expr.child_by_field_name("right") {
                    // `this.f = f`: the property holds what is assigned to it.
                    if expr.kind() == "assignment_expression"
                        && let Some(left) = expr.child_by_field_name("left")
                        && left.kind() == "member_expression"
                        && let parts = ts::children(left)
                        && parts.first().is_some_and(|o| o.kind() == "this")
                        && let Some(property) = parts.last().filter(|p| p.kind() == "property_identifier")
                        && let Some(class) = self
                            .kind_stack
                            .iter()
                            .zip(self.scope_stack.iter())
                            .rev()
                            .find(|(kind, _)| **kind == NodeKind::Class)
                            .map(|(_, qname)| qname.clone())
                    {
                        let attribute = node_id(
                            self.project_name,
                            &format!("{class}.{}", self.text(*property)),
                            NodeKind::Attribute.as_str(),
                        );
                        if self.graph.has_node(&attribute) {
                            self.record_aliases(rhs, &attribute, 0);
                        }
                    }
                    if let Some(left) = expr.child_by_field_name("left") {
                        self.scan_assignment_target(left, enclosing_id)?;
                    }
                    self.scan_value(rhs, enclosing_id)?;
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
        if let Some(definition) = ts_node {
            crate::doc::add_comments(self.py, &metadata, definition, self.source)?;
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

/// The identifiers of a namespace name in source order: `A` is [A], `A.B.C` is [A, B, C].
fn namespace_segments<'t>(name: TsNode<'t>, out: &mut Vec<TsNode<'t>>) {
    match name.kind() {
        "nested_identifier" | "member_expression" => {
            if let Some(object) = name.child_by_field_name("object") {
                namespace_segments(object, out);
            }
            if let Some(property) = name.child_by_field_name("property") {
                out.push(property);
            }
        }
        _ => out.push(name),
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
            // `a.Shape` in `interface J extends a.Shape`: the name, not the namespace.
            "nested_type_identifier" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    nodes.push(name_node);
                }
            }
            "generic_type" => {
                let head = child
                    .child_by_field_name("name")
                    .or_else(|| ts::child_of_types(child, &["type_identifier", "identifier"]));
                let head = match head {
                    Some(h) if h.kind() == "nested_type_identifier" => {
                        h.child_by_field_name("name")
                    }
                    other => other,
                };
                if let Some(name_node) = head {
                    nodes.push(name_node);
                }
            }
            _ => {}
        }
    }
    nodes
}
