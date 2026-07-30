//! Structural extraction from PHP: namespaces, classes, functions, imports.
//!
//! The state is the three stacks the Python and TypeScript visitors use:
//! `scope_stack` (the qualified-name prefix), `container_stack` (the parent's
//! id) and `kind_stack` (to tell METHOD from FUNCTION). The separator is a
//! backslash at every level — PHP writes `::` only in expressions, never in a
//! qualified name — so a method of `App\Model\User` is `App\Model\User\save`.
//!
//! Unlike the other four languages, the walk is a *single* dispatcher rather
//! than a declaration pass plus a value pass. PHP admits a class or a function
//! wherever a statement is admitted — inside another function's body, inside
//! an `if`, inside a closure — so two passes would either miss those or scan
//! them twice. One table handles both, and every occurrence is enclosed by
//! `container()`, which the declaration arms keep current.
//!
//! The visitor creates no CALLS/INHERITS_FROM/REFERENCES/HAS_TYPE edges — it
//! collects `OccurrenceRef`s, which the resolution pass binds to definitions.

use std::collections::HashSet;

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use tree_sitter::Node as TsNode;

use crate::graph::Graph;
use crate::ids::node_id;
use crate::node::{Node, NodeKind};
use crate::occurrence::OccurrenceRef;
use crate::relation::{Relation, RelationKind};
use crate::ts;

use super::deps::classify_import;

/// Which class-like declaration produced a CLASS node.
///
/// All five share one NodeKind because they share one namespace, one set of
/// member kinds and one inheritance mechanism; the metadata booleans are what
/// a consumer filters on.
#[derive(Clone, Copy)]
enum ClassFlavour {
    Class,
    Interface,
    Trait,
    Enum,
    Anonymous,
}

/// Children with `comment` and `text_interpolation` removed.
///
/// Both are `extras` in this grammar, which means the parser may put them
/// between *any* two children of *any* node — including between two methods
/// of a class, and between the two halves of `Foo::BAR`. Every read that
/// depends on a child's position has to go through here, or a comment in the
/// wrong place silently changes what "the first child" means.
fn significant<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    ts::children(node)
        .into_iter()
        .filter(|c| !matches!(c.kind(), "comment" | "text_interpolation"))
        .collect()
}

/// The final `name` of a possibly qualified reference: `App\Model\User` →
/// `User`.
///
/// `qualified_name` and `relative_name` keep their namespace in a `prefix`
/// field, so the bare identifier is the *last* `name` among the direct
/// children — the prefix's own segments sit one level deeper.
fn last_name<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    if node.kind() == "name" {
        return Some(node);
    }
    ts::children(node)
        .into_iter()
        .rev()
        .find(|c| c.kind() == "name")
}

/// The bare identifier behind a variable: `$user` → `user`.
///
/// `by_ref` wraps the variable in `&$user`, which a promoted constructor
/// parameter accepts, so the wrapper is unwrapped before the `name` token is
/// taken. `$this` needs no special case — the grammar has no dedicated node
/// for it, only a `variable_name` whose identifier reads `this`.
fn variable_identifier<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    let variable = if node.kind() == "by_ref" {
        ts::child_of_type(node, "variable_name")?
    } else {
        node
    };
    ts::child_of_type(variable, "name")
}

/// Every class name a type or a heritage clause mentions.
fn type_refs<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    collect_type_refs(node, &mut out);
    out
}

/// Class-name leaves of a type subtree.
///
/// `type` is a supertype: `kind()` never returns `"type"` but one of
/// `named_type`, `optional_type`, `union_type`, `intersection_type`,
/// `disjunctive_normal_form_type`, `primitive_type` or `bottom_type`. One
/// recursive walk therefore covers every composite form, instead of a branch
/// per shape that a new PHP release can add to.
///
/// In a type position the class sits inside a `named_type`; in `extends`,
/// `implements` and a trait `use` it is a raw name with no wrapper at all.
/// Both reduce to the same leaf here.
fn collect_type_refs<'tree>(node: TsNode<'tree>, out: &mut Vec<TsNode<'tree>>) {
    match node.kind() {
        "name" => out.push(node),
        "qualified_name" | "relative_name" => {
            if let Some(name_node) = last_name(node) {
                out.push(name_node);
            }
        }
        // `int`, `string`, `never`, `void`: a built-in names nothing the
        // resolver could bind, and it wraps nothing worth descending into.
        "primitive_type" | "bottom_type" => {}
        _ => {
            for child in ts::children(node) {
                collect_type_refs(child, out);
            }
        }
    }
}

/// A member's read visibility, `public` when nothing says otherwise.
///
/// PHP 8.4 allows two modifiers on one member — `public private(set) string
/// $x` — where the parenthesised one constrains writes only. Taking the last
/// modifier, or the whole text of the first, reports that property as private
/// to every reader.
fn visibility(node: TsNode<'_>, source: &[u8]) -> String {
    for child in ts::children(node) {
        if child.kind() != "visibility_modifier" {
            continue;
        }
        // The asymmetric form carries an `operation` child; the plain one
        // does not, and only the plain one describes read access.
        if ts::has_child_of_type(child, "operation") {
            continue;
        }
        // Keywords are case-insensitive in PHP, so `PUBLIC` and `public` must
        // not become two different metadata values.
        return ts::text(child, source).to_lowercase();
    }
    "public".to_string()
}

/// What a `use` statement imports: `class`, `function` or `const`.
///
/// The `type:` field holds an ANONYMOUS `function`/`const` token, absent from
/// `named_children`, which makes this the only place the distinction is
/// visible. Without it `use function App\slugify;` is indistinguishable from
/// a class import and the resolver looks for the wrong kind of symbol.
fn use_kind(node: TsNode<'_>) -> &'static str {
    match node.child_by_field_name("type").map(|n| n.kind()) {
        Some("function") => "function",
        Some("const") => "const",
        _ => "class",
    }
}

/// The namespace a file declares, or `""` for the global one.
///
/// PSR-4 puts one namespace in a file and the first `namespace_definition` is
/// taken as the file's own — the adapter needs it before the walk, to build
/// the MODULE chain the FILE node hangs off. Both spellings are real:
/// `namespace App;` has no `body`, `namespace App { … }` has no terminator,
/// and `namespace { … }` has no name at all.
pub fn php_namespace(root: TsNode<'_>, source: &[u8]) -> String {
    for child in ts::children(root) {
        if child.kind() == "namespace_definition"
            && let Some(name) = child.child_by_field_name("name")
        {
            return ts::text(name, source).trim_matches('\\').to_string();
        }
    }
    String::new()
}

/// Whether the file holds any PHP at all.
///
/// A `.php` file that forgot its opening tag parses to `(program (text))`
/// with no error at all — a silent total loss — and a legitimate pure-HTML
/// template parses to exactly the same shape. A caller must therefore treat
/// "no statements" as "nothing to read here", never as a parse failure.
pub fn has_php_statements(root: TsNode<'_>) -> bool {
    ts::children(root).iter().any(|child| {
        !matches!(
            child.kind(),
            "text" | "php_tag" | "php_end_tag" | "comment" | "text_interpolation"
        )
    })
}

pub struct PhpVisitor<'a> {
    py: Python<'a>,
    graph: &'a mut Graph,
    source: &'a [u8],
    project_name: &'a str,
    /// Relative to the project root. An absolute path here breaks both the
    /// span index and the determinism of everything downstream of it.
    file_path: &'a str,
    file_node_id: &'a str,
    /// Namespace prefixes the project declares (PSR-4) and Composer vendor
    /// prefixes — see `deps::classify_import` for why PHP cannot classify an
    /// import from the manifest alone.
    internal: &'a HashSet<String>,
    vendors: &'a HashSet<String>,
    /// Namespace → MODULE node id, accumulated by the adapter across files.
    /// A file can only bind an import to a namespace the adapter has already
    /// seen, so the map comes from outside rather than from the graph.
    modules: &'a IndexMap<String, String>,
    scope_stack: Vec<String>,
    container_stack: Vec<String>,
    kind_stack: Vec<NodeKind>,
    occurrences: Vec<OccurrenceRef>,
}

impl<'a> PhpVisitor<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        py: Python<'a>,
        graph: &'a mut Graph,
        source: &'a [u8],
        project_name: &'a str,
        file_path: &'a str,
        namespace: &'a str,
        file_node_id: &'a str,
        internal: &'a HashSet<String>,
        vendors: &'a HashSet<String>,
        modules: &'a IndexMap<String, String>,
    ) -> Self {
        Self {
            py,
            graph,
            source,
            project_name,
            file_path,
            file_node_id,
            internal,
            vendors,
            modules,
            scope_stack: vec![namespace.to_string()],
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

    /// A name inside the current scope.
    fn qualify(&self, name: &str) -> String {
        let scope = self.scope();
        if scope.is_empty() {
            name.to_string()
        } else {
            format!("{scope}\\{name}")
        }
    }

    // -- dispatch ---------------------------------------------------------

    /// The one dispatcher: declarations, expressions and everything between.
    ///
    /// The arms are grouped as a table on purpose. Declarations come first
    /// because they may appear in either position; expressions follow; the
    /// fallback walks children, which is what descends through `program`,
    /// `compound_statement`, `if_statement` and the rest of the control-flow
    /// grammar without naming any of it.
    pub fn visit(&mut self, node: TsNode<'_>) -> PyResult<()> {
        match node.kind() {
            // `comment` and `text_interpolation` are `extras` and may appear
            // anywhere; `text` and the tags are the HTML a template file
            // interleaves with its PHP.
            "comment" | "text_interpolation" | "text" | "php_tag" | "php_end_tag" => Ok(()),

            // -- declarations -----------------------------------------------
            "namespace_definition" => self.visit_namespace_definition(node),
            "class_declaration" => self.handle_class(node, ClassFlavour::Class),
            "interface_declaration" => self.handle_class(node, ClassFlavour::Interface),
            "trait_declaration" => self.handle_class(node, ClassFlavour::Trait),
            "enum_declaration" => self.handle_class(node, ClassFlavour::Enum),
            "function_definition" | "method_declaration" => self.handle_function(node),
            "property_declaration" => self.visit_property_declaration(node),
            "const_declaration" => self.visit_const_declaration(node),
            "enum_case" => self.visit_enum_case(node),
            "use_declaration" => self.visit_trait_use(node),
            "namespace_use_declaration" => self.visit_namespace_use_declaration(node),
            "function_static_declaration" => self.visit_static_variables(node),

            // A closure, an arrow function and a property hook are callables
            // with no qualified name of their own.
            "anonymous_function" | "arrow_function" | "property_hook" => {
                self.visit_unnamed_callable(node)
            }

            // -- calls ------------------------------------------------------
            "function_call_expression" => self.visit_function_call(node),
            "member_call_expression" | "nullsafe_member_call_expression"
            | "scoped_call_expression" => {
                self.record_member("call", node)?;
                if let Some(arguments) = node.child_by_field_name("arguments") {
                    self.scan_arguments(arguments)?;
                }
                Ok(())
            }
            "object_creation_expression" => self.visit_object_creation(node),
            "attribute" => self.visit_attribute(node),

            // -- accesses ---------------------------------------------------
            "member_access_expression" | "nullsafe_member_access_expression"
            | "scoped_property_access_expression" => self.record_member("read", node),
            "class_constant_access_expression" => self.visit_class_constant_access(node),

            // -- writes -----------------------------------------------------
            "assignment_expression" | "augmented_assignment_expression"
            | "reference_assignment_expression" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.scan_target(left)?;
                }
                if let Some(right) = node.child_by_field_name("right") {
                    self.visit(right)?;
                }
                Ok(())
            }
            // `$this->count++` writes through the member the way `+=` does.
            "update_expression" => match node.child_by_field_name("argument") {
                Some(argument) => self.scan_target(argument),
                None => Ok(()),
            },

            // -- plain names ------------------------------------------------
            "binary_expression" => self.visit_binary_expression(node),
            "catch_clause" => self.visit_catch_clause(node),
            "name" => {
                let container = self.container();
                self.record_occurrence("read", node, &container);
                Ok(())
            }
            "qualified_name" | "relative_name" => {
                if let Some(name_node) = last_name(node) {
                    let container = self.container();
                    self.record_occurrence("read", name_node, &container);
                }
                Ok(())
            }
            // A local variable, `$this` included: nothing outside the
            // function can name it, so there is nothing to resolve.
            "variable_name" => Ok(()),

            _ => self.visit_children(node),
        }
    }

    fn visit_children(&mut self, node: TsNode<'_>) -> PyResult<()> {
        for child in ts::children(node) {
            self.visit(child)?;
        }
        Ok(())
    }

    /// `namespace App;` and `namespace App { … }`, which behave differently.
    fn visit_namespace_definition(&mut self, node: TsNode<'_>) -> PyResult<()> {
        // `name:` is optional — `namespace { … }` puts its body in the global
        // namespace — and so is `body:`.
        let name = node
            .child_by_field_name("name")
            .map(|n| self.text(n).trim_matches('\\').to_string())
            .unwrap_or_default();

        match node.child_by_field_name("body") {
            // The braces bound the scope, so several namespaces in one file
            // each qualify only their own declarations.
            Some(body) => {
                let container = self.container();
                let kind = self.kind_stack.last().copied().unwrap_or(NodeKind::File);
                self.push(name, container, kind);
                let result = self.visit_children(body);
                self.pop();
                result
            }
            // The statement form runs to the end of the file or to the next
            // such statement, which is why it rewrites the current scope
            // instead of pushing: the adapter seeded the stack with the
            // file's first namespace, and a second statement takes over from
            // where it stands.
            None => {
                if let Some(scope) = self.scope_stack.last_mut() {
                    *scope = name;
                }
                Ok(())
            }
        }
    }

    // -- classes, interfaces, traits, enums --------------------------------

    fn handle_class(&mut self, node: TsNode<'_>, flavour: ClassFlavour) -> PyResult<()> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = self.qualify(&name);
        self.declare_class(node, &qname, &name, Some(name_node), flavour)
    }

    /// `new class(…) extends Base { … }`.
    ///
    /// The node has no `name` field, and its position is the only stable
    /// identity available — but it has to have one: without a CLASS node its
    /// methods would be qualified into the enclosing scope, where they could
    /// collide with a real member of it.
    fn handle_anonymous_class(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let span = ts::span(node);
        let name = format!("class@{}:{}", span.start_line, span.start_col);
        let qname = self.qualify(&name);
        self.declare_class(node, &qname, &name, None, ClassFlavour::Anonymous)?;

        // The constructor arguments belong to the code that wrote `new`, not
        // to the class body, so they are scanned once the class scope is
        // popped again.
        if let Some(arguments) = ts::child_of_type(node, "arguments") {
            self.scan_arguments(arguments)?;
        }
        Ok(())
    }

    fn declare_class(
        &mut self,
        node: TsNode<'_>,
        qname: &str,
        name: &str,
        name_node: Option<TsNode<'_>>,
        flavour: ClassFlavour,
    ) -> PyResult<()> {
        let metadata = PyDict::new(self.py);
        metadata.set_item("is_interface", matches!(flavour, ClassFlavour::Interface))?;
        metadata.set_item("is_trait", matches!(flavour, ClassFlavour::Trait))?;
        metadata.set_item("is_enum", matches!(flavour, ClassFlavour::Enum))?;
        metadata.set_item("is_anonymous", matches!(flavour, ClassFlavour::Anonymous))?;
        metadata.set_item("is_abstract", ts::has_child_of_type(node, "abstract_modifier"))?;

        let class_node =
            self.make_node(NodeKind::Class, qname, name, Some(node), metadata, name_node)?;
        let class_id = class_node.get().id.clone();
        self.add_node_with_relation(class_node, RelationKind::Declares)?;

        // Pushed before the members are read so everything below — including
        // the `#[Attr]` groups and the base list — encloses to the class.
        self.push(qname.to_string(), class_id, NodeKind::Class);
        self.visit_attributes(node)?;

        // `extends` and `implements` only. An enum's backing type
        // (`enum Status: string`) is a POSITIONAL `primitive_type` child
        // sharing the child list with `class_interface_clause`, so a scan of
        // the whole child list would report `string` as an implemented
        // interface.
        let container = self.container();
        for clause in significant(node) {
            if !matches!(clause.kind(), "base_clause" | "class_interface_clause") {
                continue;
            }
            for name_ref in type_refs(clause) {
                self.record_occurrence("base", name_ref, &container);
            }
        }

        if let Some(body) = node.child_by_field_name("body") {
            self.visit_children(body)?;
        }
        self.pop();
        Ok(())
    }

    /// `use SomeTrait;` inside a class body, recorded as a `base`: a trait
    /// contributes members to a class the way a parent class does.
    ///
    /// The conflict-resolution block — `use A, B { A::f insteadof B; B::f as
    /// g; }` — names traits *and* their methods with no field names to tell
    /// the two apart. Reading the whole subtree as type references, which is
    /// what graphlens does, turns every method named there into a phantom
    /// base class.
    fn visit_trait_use(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let container = self.container();
        for child in ts::children(node) {
            match child.kind() {
                "name" | "qualified_name" | "relative_name" => {
                    if let Some(name_node) = last_name(child) {
                        self.record_occurrence("base", name_node, &container);
                    }
                }
                "use_list" => self.visit_trait_use_list(child, &container),
                _ => {}
            }
        }
        Ok(())
    }

    fn visit_trait_use_list(&mut self, list: TsNode<'_>, container: &str) {
        for clause in ts::children(list) {
            let is_instead_of = clause.kind() == "use_instead_of_clause";
            if !is_instead_of && clause.kind() != "use_as_clause" {
                continue;
            }
            // Positional: the operand before the operator is the method being
            // renamed or excluded, everything after it is either another
            // trait (`insteadof`) or a new local name (`as`).
            let mut before_operator = true;
            for part in significant(clause) {
                match part.kind() {
                    // `A::f` — the qualifier is a used trait, the trailing
                    // name one of its methods.
                    "class_constant_access_expression" => {
                        let parts = significant(part);
                        if let Some(&qualifier) = parts.first()
                            && let Some(trait_name) = last_name(qualifier)
                        {
                            self.record_occurrence("base", trait_name, container);
                        }
                        if let Some(&method) = parts.last()
                            && method.kind() == "name"
                        {
                            self.record_occurrence("read", method, container);
                        }
                        before_operator = false;
                    }
                    // `f as g;` where only one trait is used: the method.
                    "name" if before_operator => {
                        self.record_occurrence("read", part, container);
                        before_operator = false;
                    }
                    // `insteadof B, C` excludes further traits. An `as` alias
                    // introduces a name that refers to nothing yet, so it is
                    // deliberately not recorded.
                    "name" if is_instead_of => {
                        self.record_occurrence("base", part, container);
                    }
                    _ => {}
                }
            }
        }
    }

    // -- functions and methods ---------------------------------------------

    fn handle_function(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = self.qualify(&name);
        let kind = if self.kind_stack.last() == Some(&NodeKind::Class) {
            NodeKind::Method
        } else {
            NodeKind::Function
        };

        let metadata = PyDict::new(self.py);
        metadata.set_item("is_static", ts::has_child_of_type(node, "static_modifier"))?;
        metadata.set_item("is_abstract", ts::has_child_of_type(node, "abstract_modifier"))?;
        metadata.set_item("visibility", visibility(node, self.source))?;

        let func_node =
            self.make_node(kind, &qname, &name, Some(node), metadata, Some(name_node))?;
        let func_id = func_node.get().id.clone();
        self.add_node_with_relation(func_node, RelationKind::Declares)?;

        // The owning class is read before the push, so a promoted
        // constructor parameter can be declared as its property.
        let owner = self.enclosing_class();

        self.push(qname.clone(), func_id.clone(), kind);
        self.visit_attributes(node)?;
        if let Some(return_type) = node.child_by_field_name("return_type") {
            self.record_type(return_type, &func_id);
        }
        if let Some(params) = node.child_by_field_name("parameters") {
            self.extract_parameters(params, &func_id, &qname, owner.as_ref())?;
        }
        // Absent on an abstract method and on an interface's members.
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_children(body)?;
        }
        self.pop();
        Ok(())
    }

    /// `(qualified name, node id)` of the class currently being declared.
    fn enclosing_class(&self) -> Option<(String, String)> {
        if self.kind_stack.last() != Some(&NodeKind::Class) {
            return None;
        }
        Some((self.scope().to_string(), self.container()))
    }

    fn extract_parameters(
        &mut self,
        params_node: TsNode<'_>,
        function_id: &str,
        function_qname: &str,
        owner: Option<&(String, String)>,
    ) -> PyResult<()> {
        for child in ts::children(params_node) {
            // `property_promotion_parameter` is a kind of its own, not a
            // `simple_parameter` with a modifier, and it is simultaneously a
            // parameter and a property.
            let is_promoted = child.kind() == "property_promotion_parameter";
            let is_plain = matches!(child.kind(), "simple_parameter" | "variadic_parameter");
            if !is_promoted && !is_plain {
                continue;
            }
            let Some(name_node) = child.child_by_field_name("name") else {
                continue;
            };
            let Some(ident) = variable_identifier(name_node) else {
                continue;
            };
            let param_name = self.text(ident);
            let type_node = child.child_by_field_name("type");
            let default = child.child_by_field_name("default_value");

            let metadata = PyDict::new(self.py);
            metadata.set_item("is_variadic", child.kind() == "variadic_parameter")?;
            metadata.set_item("is_promoted", is_promoted)?;
            metadata.set_item("has_default", default.is_some())?;

            // A promoted parameter and the property it declares are written
            // by the same `$name` token, and a position lookup can only
            // return one of them. The property wins: it is what the rest of
            // the class refers to as `$this->name`, while a PHP parameter is
            // a local that no occurrence ever points at.
            let param_qname = format!("{function_qname}\\{param_name}");
            let param_node = self.make_node(
                NodeKind::Parameter,
                &param_qname,
                &param_name,
                Some(child),
                metadata,
                (!is_promoted).then_some(ident),
            )?;
            let param_id = param_node.get().id.clone();
            self.safe_add_node(param_node);
            self.push_relation(
                function_id.to_string(),
                param_id.clone(),
                RelationKind::Declares,
            )?;

            self.visit_attributes(child)?;
            self.visit_property_hooks(child)?;
            if let Some(type_node) = type_node {
                self.record_type(type_node, &param_id);
            }
            if let Some(default) = default {
                self.visit(default)?;
            }
            if is_promoted
                && let Some((class_qname, class_id)) = owner
            {
                self.declare_promoted_property(child, ident, class_qname, class_id)?;
            }
        }
        Ok(())
    }

    /// The property half of a promoted constructor parameter.
    fn declare_promoted_property(
        &mut self,
        param: TsNode<'_>,
        ident: TsNode<'_>,
        class_qname: &str,
        class_id: &str,
    ) -> PyResult<()> {
        let name = self.text(ident);
        let metadata = PyDict::new(self.py);
        // The field is named `visibility` here rather than being an unnamed
        // modifier child, but the child's kind is `visibility_modifier`
        // either way, so one reader serves both spellings.
        metadata.set_item("visibility", visibility(param, self.source))?;
        metadata.set_item("is_static", false)?;
        metadata.set_item("is_readonly", ts::has_child_of_type(param, "readonly_modifier"))?;
        metadata.set_item("is_promoted", true)?;

        let qname = format!("{class_qname}\\{name}");
        let prop_node = self.make_node(
            NodeKind::Attribute,
            &qname,
            &name,
            Some(param),
            metadata,
            Some(ident),
        )?;
        let prop_id = prop_node.get().id.clone();
        self.safe_add_node(prop_node);
        self.push_relation(class_id.to_string(), prop_id.clone(), RelationKind::Declares)?;
        if let Some(type_node) = param.child_by_field_name("type") {
            self.record_type(type_node, &prop_id);
        }
        Ok(())
    }

    /// A closure, an arrow function or a PHP 8.4 property hook.
    ///
    /// No PARAMETER nodes: there is no qualified name to hang them off. Their
    /// types are still recorded, attributed to the enclosing definition,
    /// which is where a reader would look for them anyway. A property hook's
    /// own `get`/`set` is a keyword in that position and not a symbol, which
    /// is why the walk starts at the fields rather than at the children.
    fn visit_unnamed_callable(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let enclosing = self.container();
        if let Some(params) = node.child_by_field_name("parameters") {
            for param in ts::children(params) {
                if let Some(type_node) = param.child_by_field_name("type") {
                    self.record_type(type_node, &enclosing);
                }
                if let Some(default) = param.child_by_field_name("default_value") {
                    self.visit(default)?;
                }
            }
        }
        if let Some(return_type) = node.child_by_field_name("return_type") {
            self.record_type(return_type, &enclosing);
        }
        // `anonymous_function` and `property_hook` hold a compound statement,
        // `arrow_function` a bare expression; the dispatcher handles both.
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body)?;
        }
        Ok(())
    }

    // -- properties, constants, enum cases, static variables ---------------

    /// `public ?Logger $log = null, $other;`
    ///
    /// The declaration has no `name` field: it holds one or more
    /// `property_element` children, each with its own name and default, and
    /// the type is shared by all of them.
    fn visit_property_declaration(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let type_node = node.child_by_field_name("type");
        let member_visibility = visibility(node, self.source);
        let is_static = ts::has_child_of_type(node, "static_modifier");
        let is_readonly = ts::has_child_of_type(node, "readonly_modifier");

        for element in ts::children(node) {
            if element.kind() != "property_element" {
                continue;
            }
            let Some(name_node) = element.child_by_field_name("name") else {
                continue;
            };
            let Some(ident) = variable_identifier(name_node) else {
                continue;
            };
            let name = self.text(ident);
            let qname = self.qualify(&name);

            let metadata = PyDict::new(self.py);
            metadata.set_item("visibility", &member_visibility)?;
            metadata.set_item("is_static", is_static)?;
            metadata.set_item("is_readonly", is_readonly)?;
            metadata.set_item("is_promoted", false)?;

            let prop_node = self.make_node(
                NodeKind::Attribute,
                &qname,
                &name,
                Some(element),
                metadata,
                Some(ident),
            )?;
            let prop_id = prop_node.get().id.clone();
            self.add_node_with_relation(prop_node, RelationKind::Declares)?;

            if let Some(type_node) = type_node {
                self.record_type(type_node, &prop_id);
            }
            if let Some(default) = element.child_by_field_name("default_value") {
                self.visit(default)?;
            }
        }

        // The `#[Attr]` groups and the hooks belong to the declaration as a
        // whole, so both enclose to the class rather than to one element.
        self.visit_attributes(node)?;
        self.visit_property_hooks(node)
    }

    /// The PHP 8.4 hook block of a property or a promoted parameter.
    ///
    /// Held by the declaration, not by the `property_element` that names the
    /// property, and reached through the dispatcher so a hook's own `get`/`set`
    /// keyword is not mistaken for a symbol.
    fn visit_property_hooks(&mut self, node: TsNode<'_>) -> PyResult<()> {
        match ts::child_of_type(node, "property_hook_list") {
            Some(hooks) => self.visit_children(hooks),
            None => Ok(()),
        }
    }

    /// `const` at file scope and `const` in a class body.
    ///
    /// The grammar gives both the same `const_declaration`, so the scope
    /// stack is the only discriminator: a class constant is a member
    /// (ATTRIBUTE), a file-scope one is not (VARIABLE).
    fn visit_const_declaration(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let in_class = self.kind_stack.last() == Some(&NodeKind::Class);
        let kind = if in_class { NodeKind::Attribute } else { NodeKind::Variable };
        let type_node = node.child_by_field_name("type");

        for element in ts::children(node) {
            if element.kind() != "const_element" {
                continue;
            }
            // No field names: `[name, =, expression]`. The value may itself
            // be a `name` (`const A = B;`), so the *first* one is the name.
            let Some(name_node) = ts::child_of_type(element, "name") else {
                continue;
            };
            let name = self.text(name_node);
            let qname = self.qualify(&name);

            let metadata = PyDict::new(self.py);
            metadata.set_item("is_constant", true)?;
            // A file-scope constant has no visibility in PHP; only a class
            // constant can be anything other than public.
            if in_class {
                metadata.set_item("visibility", visibility(node, self.source))?;
            }

            let const_node = self.make_node(
                kind,
                &qname,
                &name,
                Some(element),
                metadata,
                Some(name_node),
            )?;
            let const_id = const_node.get().id.clone();
            self.add_node_with_relation(const_node, RelationKind::Declares)?;

            if let Some(type_node) = type_node {
                self.record_type(type_node, &const_id);
            }
            // Only what follows the name: scanning from the start would
            // record the constant as a reference to itself.
            let mut past_name = false;
            for child in ts::children(element) {
                if child == name_node {
                    past_name = true;
                    continue;
                }
                if past_name {
                    self.visit(child)?;
                }
            }
        }
        self.visit_attributes(node)
    }

    fn visit_enum_case(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let name = self.text(name_node);
        let qname = self.qualify(&name);

        let metadata = PyDict::new(self.py);
        metadata.set_item("is_enum_case", true)?;

        let case_node = self.make_node(
            NodeKind::Attribute,
            &qname,
            &name,
            Some(node),
            metadata,
            Some(name_node),
        )?;
        self.add_node_with_relation(case_node, RelationKind::Declares)?;

        self.visit_attributes(node)?;
        if let Some(value) = node.child_by_field_name("value") {
            self.visit(value)?;
        }
        Ok(())
    }

    /// `static $calls = 0;` inside a function body.
    ///
    /// A VARIABLE rather than an ATTRIBUTE: the binding belongs to the
    /// function, no class can be its owner, and it outlives the call the way
    /// a file-scope constant outlives a statement.
    fn visit_static_variables(&mut self, node: TsNode<'_>) -> PyResult<()> {
        for declaration in ts::children(node) {
            if declaration.kind() != "static_variable_declaration" {
                continue;
            }
            let Some(name_node) = declaration.child_by_field_name("name") else {
                continue;
            };
            let Some(ident) = variable_identifier(name_node) else {
                continue;
            };
            let name = self.text(ident);
            let qname = self.qualify(&name);

            let metadata = PyDict::new(self.py);
            metadata.set_item("is_static", true)?;

            let var_node = self.make_node(
                NodeKind::Variable,
                &qname,
                &name,
                Some(declaration),
                metadata,
                Some(ident),
            )?;
            self.add_node_with_relation(var_node, RelationKind::Declares)?;

            if let Some(value) = declaration.child_by_field_name("value") {
                self.visit(value)?;
            }
        }
        Ok(())
    }

    // -- imports -----------------------------------------------------------

    fn visit_namespace_use_declaration(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let declared_kind = use_kind(node);

        // A group import — `use App\Contracts\{Cache, Logger as Log};` —
        // keeps its prefix as a POSITIONAL `namespace_name` child of the
        // declaration while the clauses hold bare names, so the two halves
        // have to be joined by hand.
        if let Some(group) = node.child_by_field_name("body") {
            let prefix = ts::child_of_type(node, "namespace_name")
                .map(|n| self.text(n))
                .unwrap_or_default();
            for clause in ts::children(group) {
                if clause.kind() == "namespace_use_clause" {
                    self.emit_use_clause(clause, &prefix, declared_kind)?;
                }
            }
            return Ok(());
        }
        for clause in ts::children(node) {
            if clause.kind() == "namespace_use_clause" {
                self.emit_use_clause(clause, "", declared_kind)?;
            }
        }
        Ok(())
    }

    fn emit_use_clause(
        &mut self,
        clause: TsNode<'_>,
        prefix: &str,
        declared_kind: &'static str,
    ) -> PyResult<()> {
        // A plain import holds its whole path as one `qualified_name` whose
        // text already carries the separators. Reassembling it from the
        // `prefix` field instead would interleave the anonymous `\` tokens,
        // which is why the text of the node is taken as it stands.
        let Some(path_node) = ts::child_of_types(clause, &["qualified_name", "name"]) else {
            return Ok(());
        };
        let path = self.text(path_node);
        let path = path.trim_matches('\\');
        let ext_qname = if prefix.is_empty() {
            path.to_string()
        } else {
            format!("{}\\{path}", prefix.trim_matches('\\'))
        };
        if ext_qname.is_empty() {
            return Ok(());
        }

        let last_segment = ext_qname
            .rsplit('\\')
            .next()
            .unwrap_or(ext_qname.as_str())
            .to_string();
        let local = match clause.child_by_field_name("alias") {
            Some(alias) => self.text(alias),
            None => last_segment.clone(),
        };
        // A clause inside a group carries its own `type:` field:
        // `use App\{function slugify, const LIMIT, Cache};`.
        let kind = match use_kind(clause) {
            "class" => declared_kind,
            own => own,
        };
        self.emit_import(&local, &ext_qname, &last_segment, kind)
    }

    fn emit_import(
        &mut self,
        local_name: &str,
        ext_qname: &str,
        last_segment: &str,
        imported_kind: &str,
    ) -> PyResult<()> {
        let origin = classify_import(ext_qname, self.internal, self.vendors);

        let metadata = PyDict::new(self.py);
        // `alias` is set only when the local name differs from the imported
        // symbol's own last segment: that is what makes `use A\B as C;`
        // distinguishable from `use A\B;` downstream.
        metadata.set_item("alias", (local_name != last_segment).then_some(local_name))?;
        metadata.set_item("original_name", ext_qname)?;
        metadata.set_item("use_kind", imported_kind)?;
        metadata.set_item("origin", origin)?;

        let import_qname = self.qualify(local_name);
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

        // An internal import binds to the MODULE of its longest namespace
        // prefix, so `App\Model\User` lands on `App\Model` even before the
        // User class has a node of its own. Everything else becomes an
        // EXTERNAL_SYMBOL, so the edge is never lost.
        let mut target_id = if origin == "internal" {
            self.lookup_module(ext_qname)
        } else {
            None
        };
        if target_id.is_none() {
            target_id = Some(self.get_or_create_external_symbol(ext_qname, origin)?);
        }
        let target_id = target_id.expect("the external symbol is created above");

        let file_id = self.file_node_id.to_string();
        self.push_relation(file_id, target_id.clone(), RelationKind::Imports)?;
        self.push_relation(import_id, target_id, RelationKind::ResolvesTo)
    }

    /// The MODULE id for a namespace or its longest declared prefix.
    fn lookup_module(&self, qname: &str) -> Option<String> {
        let parts: Vec<&str> = qname.split('\\').collect();
        for length in (1..=parts.len()).rev() {
            let candidate = parts[..length].join("\\");
            if let Some(id) = self.modules.get(&candidate) {
                return Some(id.clone());
            }
        }
        None
    }

    // -- expressions -------------------------------------------------------

    fn visit_function_call(&mut self, node: TsNode<'_>) -> PyResult<()> {
        if let Some(function) = node.child_by_field_name("function") {
            match function.kind() {
                "name" | "qualified_name" | "relative_name" => {
                    if let Some(name_node) = last_name(function) {
                        let container = self.container();
                        self.record_occurrence("call", name_node, &container);
                    }
                }
                // Calling the result of an expression: `$factory()`,
                // `$this->make()()`, `(fn () => 1)()`.
                _ => self.visit(function)?,
            }
        }
        if let Some(arguments) = node.child_by_field_name("arguments") {
            self.scan_arguments(arguments)?;
        }
        Ok(())
    }

    /// `new Foo(…)`, `new $class(…)`, `new class { … }`.
    ///
    /// The node has no field names at all: the class reference is the first
    /// child after `new`, and the argument list has to be found by kind.
    fn visit_object_creation(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let container = self.container();
        for child in significant(node) {
            match child.kind() {
                "new" => {}
                "anonymous_class" => self.handle_anonymous_class(child)?,
                "arguments" => self.scan_arguments(child)?,
                "name" | "qualified_name" | "relative_name" => {
                    if let Some(name_node) = last_name(child) {
                        self.record_occurrence("call", name_node, &container);
                    }
                }
                _ => self.visit(child)?,
            }
        }
        Ok(())
    }

    /// `Foo::BAR`, `self::BAR`, `Foo::class`.
    ///
    /// No field names: `[qualifier, ::, name]`. `Foo::class` ends in the
    /// `class` keyword rather than a name — a type reference, which the
    /// qualifier already records — so only a real `name` becomes a read.
    fn visit_class_constant_access(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let children = significant(node);
        if let Some(&qualifier) = children.first() {
            self.visit(qualifier)?;
        }
        if let Some(&member) = children.last()
            && children.len() > 1
            && member.kind() == "name"
        {
            let container = self.container();
            self.record_occurrence("read", member, &container);
        }
        Ok(())
    }

    /// `#[Route('/x', name: 'index')]`.
    ///
    /// An attribute instantiates its class, so its name is a `call` the way
    /// `new` is. Its argument list sits under `parameters:` — the one node in
    /// this grammar that spells it that way instead of `arguments:`.
    fn visit_attribute(&mut self, node: TsNode<'_>) -> PyResult<()> {
        if let Some(name_node) =
            ts::child_of_types(node, &["name", "qualified_name", "relative_name"])
                .and_then(last_name)
        {
            let container = self.container();
            self.record_occurrence("call", name_node, &container);
        }
        if let Some(arguments) = node.child_by_field_name("parameters") {
            self.scan_arguments(arguments)?;
        }
        Ok(())
    }

    /// The `#[Attr]` groups on a declaration.
    ///
    /// Routed back through the dispatcher so the `parameters:` quirk lives in
    /// exactly one place.
    fn visit_attributes(&mut self, node: TsNode<'_>) -> PyResult<()> {
        match node.child_by_field_name("attributes") {
            Some(list) => self.visit_children(list),
            None => Ok(()),
        }
    }

    /// `$x instanceof Foo`.
    ///
    /// `instanceof` is not a node of its own: it is the `operator:` field of
    /// an ordinary `binary_expression`, an anonymous token whose spelling the
    /// source may write in any case, because every keyword in this grammar is
    /// case-insensitive. Its right operand is a type test rather than a
    /// constant read, which is what makes it an `annotation`.
    fn visit_binary_expression(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let is_instanceof = node
            .child_by_field_name("operator")
            .is_some_and(|op| op.kind().eq_ignore_ascii_case("instanceof"));
        let operand = if is_instanceof {
            node.child_by_field_name("right")
                .filter(|n| matches!(n.kind(), "name" | "qualified_name" | "relative_name"))
        } else {
            None
        };

        if let Some(operand) = operand
            && let Some(name_node) = last_name(operand)
        {
            let container = self.container();
            self.record_occurrence("annotation", name_node, &container);
            if let Some(left) = node.child_by_field_name("left") {
                self.visit(left)?;
            }
            return Ok(());
        }
        self.visit_children(node)
    }

    /// `catch (FooError | BarError $e) { … }` — a type position, so the class
    /// names are annotations rather than constant reads.
    fn visit_catch_clause(&mut self, node: TsNode<'_>) -> PyResult<()> {
        let container = self.container();
        if let Some(type_list) = node.child_by_field_name("type") {
            self.record_type(type_list, &container);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body)?;
        }
        Ok(())
    }

    /// The member half of an access or a call, plus its receiver.
    ///
    /// The receiver is `object:` on `->` and `?->` and `scope:` on `::`; both
    /// are ordinary expressions, and walking the `::` one is what records
    /// `Foo` in `Foo::bar()`.
    fn record_member(&mut self, role: &str, node: TsNode<'_>) -> PyResult<()> {
        // `Foo::$bar` spells its member with a `variable_name`, where
        // `$obj->$prop` uses that same node kind for a *computed* member
        // name. Only the scoped form may unwrap it.
        let is_scoped = node.kind() == "scoped_property_access_expression";
        let container = self.container();

        if let Some(name_node) = node.child_by_field_name("name") {
            match name_node.kind() {
                "name" => self.record_occurrence(role, name_node, &container),
                "variable_name" if is_scoped => {
                    if let Some(ident) = variable_identifier(name_node) {
                        self.record_occurrence(role, ident, &container);
                    }
                }
                _ => self.visit(name_node)?,
            }
        }
        if let Some(receiver) = node
            .child_by_field_name("object")
            .or_else(|| node.child_by_field_name("scope"))
        {
            self.visit(receiver)?;
        }
        Ok(())
    }

    /// An assignment's left-hand side.
    ///
    /// A member access here is a `write`, not a `read`. Destructuring nests —
    /// `[$a, [$b, $c]] = …` and `['k' => $v] = …` — so the walk recurses
    /// through `list_literal` and `pair` rather than stopping at the top
    /// node, and `by_ref` wraps a target in `[&$a] = …`.
    fn scan_target(&mut self, node: TsNode<'_>) -> PyResult<()> {
        match node.kind() {
            "member_access_expression" | "nullsafe_member_access_expression"
            | "scoped_property_access_expression" => self.record_member("write", node),
            "list_literal" | "pair" | "by_ref" => {
                for child in ts::children(node) {
                    self.scan_target(child)?;
                }
                Ok(())
            }
            // `$rows['k'] = …` writes through a container that is itself only
            // read, and the same holds for the result of a call.
            _ => self.visit(node),
        }
    }

    /// A call's arguments.
    ///
    /// A named argument's label (`send(to: $user)`) is a `name` node sitting
    /// where an ordinary expression would be; scanning it would invent a
    /// reference to a symbol called `to`.
    fn scan_arguments(&mut self, arguments: TsNode<'_>) -> PyResult<()> {
        for argument in ts::children(arguments) {
            let label = argument.child_by_field_name("name");
            for child in ts::children(argument) {
                if Some(child) == label {
                    continue;
                }
                self.visit(child)?;
            }
        }
        Ok(())
    }

    // -- occurrences -------------------------------------------------------

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

    /// One `annotation` per class name a type mentions, so a union type binds
    /// to every member of it rather than to its first.
    fn record_type(&mut self, type_node: TsNode<'_>, enclosing_id: &str) {
        for name_node in type_refs(type_node) {
            self.record_occurrence("annotation", name_node, enclosing_id);
        }
    }

    // -- graph plumbing ----------------------------------------------------

    fn get_or_create_external_symbol(&mut self, qname: &str, origin: &str) -> PyResult<String> {
        let sym_id = node_id(self.project_name, qname, NodeKind::ExternalSymbol.as_str());
        if !self.graph.has_node(&sym_id) {
            let metadata = PyDict::new(self.py);
            metadata.set_item("origin", origin)?;
            let node = Node {
                id: sym_id.clone(),
                kind: NodeKind::ExternalSymbol,
                qualified_name: qname.to_string(),
                name: qname.rsplit('\\').next().unwrap_or(qname).to_string(),
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
}
