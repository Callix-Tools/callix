//! Structural extraction from C and C++: records, functions, includes.
//!
//! The walk is split in a way none of the other adapters need, and the header
//! model is why. A name with external linkage is ONE node even when a header
//! declares it and a source file defines it (see `headers.rs`), so the site the
//! node is built from has to be chosen before any node is written — which means
//! every file of the root has to be read before the first node exists. Three
//! stages, in this order:
//!
//! 1. [`declared_scopes`] — the record and namespace names of one file. Cheap:
//!    it never enters a function body. The union over the root is what tells
//!    `Engine::ping` (a method) from `detail::label_for` (a namespaced
//!    function), because `qualified_identifier.scope` is reported as a
//!    `namespace_identifier` even when the scope is a class, and that class is
//!    usually declared in another file.
//! 2. [`read_file`] — the full walk, producing [`FileFacts`]: declarations,
//!    occurrences and `#include`s as plain data. No Python, no graph, no
//!    borrowing of the tree, so the caller may drop the tree and keep the facts.
//!    [`FileFacts::survey`] then feeds the `DeclLedger`.
//! 3. [`CFamilyEmitter`] — turns the facts of one file into nodes and edges,
//!    consulting the finished ledger for which site owns each node.
//!
//! Stages 2 and 3 are separate passes over the same data and NOT two separate
//! walks: `DeclLedger::owns` compares spans, so a survey that computed a span
//! differently from the emit pass would silently own nothing. One walk, one set
//! of spans.
//!
//! The visitor creates no CALLS / REFERENCES / HAS_TYPE / INHERITS_FROM edges —
//! it records occurrences, and the adapter's resolution pass binds them.

use std::collections::HashSet;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use tree_sitter::Node as TsNode;

use crate::graph::Graph;
use crate::ids::node_id;
use crate::node::{Node, NodeKind};
use crate::occurrence::OccurrenceRef;
use crate::relation::{Relation, RelationKind};
use crate::span::Span;
use crate::ts;

use super::Dialect;
use super::headers::{
    DeclKey, DeclLedger, DeclSite, Linkage, MEMBER_SEPARATOR, SCOPE_SEPARATOR, qualify,
    resolve_include,
};

/// Node kinds that hold their declarations one level down, as UNNAMED children.
///
/// This list is the single most load-bearing thing in the file. A normal header
/// is wrapped in an include guard, so `preproc_ifdef` sits between
/// `translation_unit` and every declaration in it — a walk over the translation
/// unit's own children finds ZERO declarations in essentially every real header.
/// The same shape recurs inside `field_declaration_list` and `enumerator_list`,
/// which admit `preproc_*` children too, so the descent is needed for struct and
/// enum bodies as well.
///
/// `#if 0 … #else … #endif` puts BOTH arms in the tree — tree-sitter evaluates
/// no preprocessor — and both are read. Two arms declaring the same name produce
/// the same qualified name and collapse into one node, which is the right answer
/// for the overwhelmingly common `#ifdef _WIN32` pair.
const TRANSPARENT: [&str; 8] = [
    "preproc_ifdef",
    "preproc_if",
    "preproc_elif",
    "preproc_elifdef",
    "preproc_else",
    "linkage_specification",
    "template_declaration",
    "declaration_list",
];

/// The declarations of a container, in source order, descending through
/// [`TRANSPARENT`].
///
/// A stack popped from the end, with children pushed in reverse, so an expanded
/// transparent node's contents come before the siblings that follow it.
fn declarations<'tree>(container: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = ts::children(container);
    stack.reverse();
    while let Some(node) = stack.pop() {
        if TRANSPARENT.contains(&node.kind()) {
            let mut inner = ts::children(node);
            inner.reverse();
            stack.extend(inner);
            continue;
        }
        out.push(node);
    }
    out
}

/// A node's descendants, itself included, depth-first.
fn descendants<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        out.push(current);
        stack.extend(ts::children(current));
    }
    out
}

/// Declarator kinds a chain ends at.
///
/// `identifier`, `field_identifier` and `type_identifier` are three distinct
/// leaves chosen by grammatical position — an ordinary name, a struct member, a
/// type name — and a typedef'd name is a `type_identifier` even in the middle of
/// a declarator chain, so all three have to terminate the descent.
const TERMINAL_DECLARATORS: [&str; 10] = [
    "identifier",
    "field_identifier",
    "type_identifier",
    "qualified_identifier",
    "destructor_name",
    "operator_name",
    "operator_cast",
    "structured_binding_declarator",
    "template_function",
    "template_method",
];

/// The end of a declarator chain.
struct Declared<'tree> {
    name: TsNode<'tree>,
    /// The NEAREST enclosing `function_declarator`, which is the one whose
    /// parameter list belongs to this name. `int (*g(void))(int)` nests two, and
    /// the outer one describes the function-pointer type `g` returns — taking it
    /// would give every function returning a function pointer the wrong
    /// parameters.
    function: Option<TsNode<'tree>>,
    /// Whether a pointer or a reference stands between the name and that
    /// function declarator. `int (*fp)(int)` — a variable holding a function
    /// pointer — is syntactically a `function_declarator` too and differs from
    /// `int fp(int)` in nothing else, so this is what tells the two apart.
    ///
    /// The test is the indirection rather than the parentheses around the name:
    /// `int (f)(int);` is a perfectly ordinary function declaration that only
    /// wrote its name in brackets to dodge a macro.
    indirect: bool,
}

impl<'tree> Declared<'tree> {
    /// Whether the chain declares something callable.
    ///
    /// A conversion operator has no `function_declarator` at all — its parameter
    /// list hangs off an `abstract_function_declarator` under `operator_cast` —
    /// so it is callable by its kind rather than by its shape.
    fn is_callable(&self) -> bool {
        self.name.kind() == "operator_cast" || (self.function.is_some() && !self.indirect)
    }

    /// The declarator carrying the parameter list and the trailing qualifiers.
    fn signature_owner(&self) -> Option<TsNode<'tree>> {
        if self.name.kind() == "operator_cast" {
            return self.name.child_by_field_name("declarator");
        }
        self.function
    }
}

/// One step down a declarator chain.
///
/// Most wrappers name their payload `declarator:`. `reference_declarator`,
/// `parenthesized_declarator`, `attributed_declarator` and `variadic_declarator`
/// do not — theirs is an unnamed child — so the fallback takes the first named
/// child that is not an attribute or a qualifier sharing the child list with it.
fn inner_declarator<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    if let Some(inner) = node.child_by_field_name("declarator") {
        return Some(inner);
    }
    ts::named_children(node).into_iter().find(|child| {
        !matches!(
            child.kind(),
            "attribute_declaration"
                | "attribute_specifier"
                | "type_qualifier"
                | "ms_call_modifier"
                | "ms_pointer_modifier"
        )
    })
}

/// The name a declarator chain declares, following the chain to its leaf.
fn terminal_declarator<'tree>(declarator: TsNode<'tree>) -> Option<Declared<'tree>> {
    let mut current = declarator;
    let mut function = None;
    let mut indirect = false;
    // A malformed tree could in principle cycle; the bound is the depth of any
    // declarator a human writes, several times over.
    for _ in 0..64 {
        if TERMINAL_DECLARATORS.contains(&current.kind()) {
            return Some(Declared { name: current, function, indirect });
        }
        match current.kind() {
            // Entering a nested function declarator restarts both: its
            // parameters are the ones the name inside it owns, and a pointer
            // passed on the way in belonged to the RETURN type.
            "function_declarator" | "abstract_function_declarator" => {
                function = Some(current);
                indirect = false;
            }
            "pointer_declarator" | "reference_declarator" | "abstract_pointer_declarator"
            | "abstract_reference_declarator" => {
                indirect = indirect || function.is_some();
            }
            _ => {}
        }
        current = inner_declarator(current)?;
    }
    None
}

/// The scope a declarator spells, and the bare name at the end of it.
///
/// `qualified_identifier` NESTS: `n::C<int>::f` is
/// `qualified_identifier{scope: n, name: qualified_identifier{scope: C<int>,
/// name: f}}`. A `template_type` scope contributes its `name` alone, dropping
/// the arguments, so an out-of-line `C<T>::f` meets the in-class `C::f` instead
/// of splitting off a second node per instantiation spelling.
fn split_qualified<'tree>(
    node: TsNode<'tree>,
    source: &[u8],
) -> (Vec<String>, TsNode<'tree>) {
    let mut scope: Vec<String> = Vec::new();
    let mut current = node;
    while current.kind() == "qualified_identifier" {
        if let Some(part) = current.child_by_field_name("scope") {
            let named = match part.kind() {
                "template_type" => part.child_by_field_name("name"),
                _ => Some(part),
            };
            if let Some(named) = named {
                let text = ts::text(named, source);
                if !text.is_empty() {
                    scope.push(text.into_owned());
                }
            }
        }
        // `::global_fn` has no scope at all and must not loop forever.
        match current.child_by_field_name("name") {
            Some(name) => current = name,
            None => return (scope, current),
        }
    }
    (scope, current)
}

/// A name stripped of its template arguments.
fn bare_name<'tree>(node: TsNode<'tree>) -> TsNode<'tree> {
    match node.kind() {
        "template_function" | "template_method" | "template_type" => {
            node.child_by_field_name("name").unwrap_or(node)
        }
        _ => node,
    }
}

/// The identifier a type reduces to, or None for a built-in.
///
/// The leading `type_identifier` of the subtree: `const engine_t *`,
/// `std::vector<Engine>` and `struct engine` all reduce to the one name the
/// resolver can bind. `int`, `unsigned long` and `auto` have none —
/// `sized_type_specifier` spells its keywords as ANONYMOUS children and may have
/// no `type` field at all — and bind to nothing, which is correct.
fn type_name_node<'tree>(type_node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    if type_node.kind() == "type_identifier" {
        return Some(type_node);
    }
    ts::children(type_node).into_iter().find_map(type_name_node)
}

/// Whether the node is what an assignment or an increment writes through.
fn is_write_target(node: TsNode<'_>) -> bool {
    node.parent().is_some_and(|parent| match parent.kind() {
        "assignment_expression" => parent.child_by_field_name("left") == Some(node),
        "update_expression" => parent.child_by_field_name("argument") == Some(node),
        _ => false,
    })
}

/// Whether a `field_expression` sits in callee position.
fn is_callee(node: TsNode<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "call_expression"
            && parent.child_by_field_name("function") == Some(node)
    })
}

/// A type reduced to one spelling.
///
/// Whitespace is collapsed and then dropped wherever it does not separate two
/// word characters, so `const char *` and `const char*` — the same parameter
/// type spelled two ways in a header and in the source file that defines the
/// function — normalize alike. That matters for more than tidiness: a C++
/// overload's parameter list is part of its qualified name, hence part of its
/// node id, and two spellings would be two nodes.
fn normalize_type(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            pending = !out.is_empty();
            continue;
        }
        if pending {
            let previous = out.chars().next_back().unwrap_or(' ');
            if is_word(previous) && is_word(ch) {
                out.push(' ');
            }
            pending = false;
        }
        out.push(ch);
    }
    out
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// The specifiers a declaration carries.
///
/// Every one of them is an UNNAMED child of the declaration rather than a field,
/// and in C both `static` and `inline` are a `storage_class_specifier`, so
/// `static inline` yields two of them. Position therefore says nothing and the
/// text is the only reliable test.
#[derive(Clone, Copy, Default)]
struct Modifiers {
    is_static: bool,
    is_extern: bool,
    is_const: bool,
    is_virtual: bool,
    is_template: bool,
}

impl Modifiers {
    fn read(node: TsNode<'_>, source: &[u8]) -> Self {
        let mut modifiers = Self {
            is_template: node
                .parent()
                .is_some_and(|parent| parent.kind() == "template_declaration"),
            ..Self::default()
        };
        for child in ts::children(node) {
            match child.kind() {
                "storage_class_specifier" => {
                    // Bound rather than matched in place: the `Cow` would be a
                    // temporary and `trim` borrows from it.
                    let text = ts::text(child, source);
                    match text.trim() {
                        "static" => modifiers.is_static = true,
                        "extern" => modifiers.is_extern = true,
                        _ => {}
                    }
                }
                "type_qualifier" => {
                    let text = ts::text(child, source);
                    if matches!(text.trim(), "const" | "constexpr") {
                        modifiers.is_const = true;
                    }
                }
                "virtual" | "virtual_function_specifier" => modifiers.is_virtual = true,
                _ => {}
            }
        }
        modifiers
    }
}

/// What DECLARES a found declaration, and what an occurrence is enclosed by.
///
/// A qualified name plus a kind rather than a node id: the walk produces no ids,
/// because `node_id` needs the project name and that belongs to the emit pass.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Owner {
    /// The file itself.
    File,
    Decl { qualified_name: String, kind: NodeKind },
}

/// One declaration, as read from one site.
///
/// A prototype in a header and its definition in a source file each produce one
/// of these with the SAME `qualified_name` and `kind`; `DeclLedger` decides
/// which of the two the node is built from.
pub struct FoundDecl {
    pub kind: NodeKind,
    pub qualified_name: String,
    /// The short name, without the parameter list a C++ qualified name carries.
    pub name: String,
    pub span: Span,
    pub name_span: Span,
    /// Whether this site is the definition: a function with a body, a record
    /// with a member list, a variable that is not merely `extern`.
    pub defines: bool,
    pub linkage: Linkage,
    pub owner: Owner,
    /// `struct` | `union` | `class` | `enum` on a CLASS node, `""` elsewhere.
    pub record_kind: &'static str,
    /// `public` | `protected` | `private` inside a class body, `""` outside one.
    pub access: &'static str,
    pub is_static: bool,
    pub is_extern: bool,
    pub is_virtual: bool,
    pub is_template: bool,
    /// The declared type, verbatim; the underlying type of a typedef.
    pub type_text: String,
    /// The normalized parameter list of a C++ callable, already folded into
    /// `qualified_name`; empty in C, which has no overloading.
    pub signature: String,
}

/// A use-site, before it has a node id to hang off.
pub struct FoundOccurrence {
    pub role: &'static str,
    pub span: Span,
    pub owner: Owner,
    /// The identifier as written. C and C++ resolve by declaration visibility
    /// rather than by position, so the resolver needs the name itself — unlike
    /// the four languages whose backends answer "what is defined at this
    /// position?" and hand back a location.
    pub name: String,
}

/// One `#include`.
pub struct FoundInclude {
    /// The path as written between the delimiters — `stddef.h`, `engine.h`, or
    /// the macro's text when the path is computed.
    pub path: String,
    /// `system` for `<…>`, `quoted` for `"…"`, `computed` for a macro.
    pub style: &'static str,
    pub span: Span,
}

/// Everything one file contributes, as plain data.
pub struct FileFacts {
    /// Project-relative, always. An absolute path here would reach a qualified
    /// name and make every id in the graph depend on where the checkout lives.
    pub file_rel: String,
    pub declarations: Vec<FoundDecl>,
    pub occurrences: Vec<FoundOccurrence>,
    pub includes: Vec<FoundInclude>,
}

impl FileFacts {
    /// Records every declaring site of the file in the ledger.
    ///
    /// Runs for every file of the root before the first node is written; see the
    /// module header for why that ordering is not an optimization.
    pub fn survey(&self, ledger: &mut DeclLedger) {
        for decl in &self.declarations {
            ledger.record(
                (decl.kind, decl.qualified_name.clone()),
                DeclSite {
                    file_rel: self.file_rel.clone(),
                    span: decl.span,
                    name_span: decl.name_span,
                    defines: decl.defines,
                },
            );
        }
    }
}

/// The scope names one file declares.
#[derive(Default)]
pub struct DeclaredScopes {
    /// Bare names of every class, struct and union — the set that classifies
    /// `X::y` as a METHOD rather than a namespaced FUNCTION. Enums are left out
    /// on purpose: nothing can define a function inside one.
    pub records: Vec<String>,
    /// Namespace paths, outermost first (`fixture`, `fixture::detail`). The
    /// adapter builds a MODULE node per entry — a C++ MODULE is a namespace.
    pub namespaces: Vec<String>,
}

/// Stage 1: the record and namespace names of one file.
///
/// Deliberately shallow — it never enters a function body and never reads a
/// declarator chain — because it runs over every file of the root before
/// anything else happens.
pub fn declared_scopes(root: TsNode<'_>, source: &[u8]) -> DeclaredScopes {
    let mut found = DeclaredScopes::default();
    collect_scopes(root, source, &[], &mut found);
    found
}

fn collect_scopes(
    container: TsNode<'_>,
    source: &[u8],
    namespace: &[String],
    found: &mut DeclaredScopes,
) {
    for node in declarations(container) {
        match node.kind() {
            "namespace_definition" => {
                let mut inner = namespace.to_vec();
                inner.extend(namespace_components(node, source));
                if !inner.is_empty() {
                    let path = inner.join(SCOPE_SEPARATOR);
                    if !found.namespaces.contains(&path) {
                        found.namespaces.push(path);
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    collect_scopes(body, source, &inner, found);
                }
            }
            "struct_specifier" | "union_specifier" | "class_specifier" => {
                collect_record_scope(node, source, namespace, found);
            }
            // A nested class reaches us as the `type` of a member declaration,
            // and a file-scope `struct engine { … } e;` the same way.
            "declaration" | "field_declaration" | "type_definition" => {
                if let Some(inner) = node.child_by_field_name("type")
                    && matches!(
                        inner.kind(),
                        "struct_specifier" | "union_specifier" | "class_specifier"
                    )
                {
                    collect_record_scope(inner, source, namespace, found);
                }
            }
            _ => {}
        }
    }
}

fn collect_record_scope(
    node: TsNode<'_>,
    source: &[u8],
    namespace: &[String],
    found: &mut DeclaredScopes,
) {
    if let Some(name) = node.child_by_field_name("name") {
        let text = ts::text(name, source).into_owned();
        if !text.is_empty() && !found.records.contains(&text) {
            found.records.push(text);
        }
    }
    if let Some(body) = node.child_by_field_name("body") {
        collect_scopes(body, source, namespace, found);
    }
}

/// The components a `namespace` clause names; empty for an anonymous one.
///
/// `namespace a::b { … }` spells them as a `nested_namespace_specifier`, whose
/// components are unnamed children.
fn namespace_components(node: TsNode<'_>, source: &[u8]) -> Vec<String> {
    let Some(name) = node.child_by_field_name("name") else {
        return Vec::new();
    };
    if name.kind() == "nested_namespace_specifier" {
        return ts::named_children(name)
            .into_iter()
            .map(|part| ts::text(part, source).into_owned())
            .filter(|text| !text.is_empty())
            .collect();
    }
    let text = ts::text(name, source).into_owned();
    if text.is_empty() { Vec::new() } else { vec![text] }
}

/// One level of the scope stack.
struct Frame {
    /// How many components this frame pushed onto the qualified-name prefix.
    /// `namespace a::b` pushes two, an anonymous namespace none.
    parts: usize,
    /// What declarations here hang off, besides the file.
    owner: Owner,
    linkage: Linkage,
    /// The access level a member declared here has, `""` outside a class body.
    access: &'static str,
    /// Whether declarations here are class members.
    member: bool,
}

/// A callable's identity, derived once and used by both its node and the walk of
/// its body.
struct CallableId {
    kind: NodeKind,
    qualified_name: String,
    /// The short name, without the parameter list the qualified name carries.
    name: String,
    name_span: Span,
    linkage: Linkage,
    signature: String,
    modifiers: Modifiers,
}

struct Reader<'a> {
    source: &'a [u8],
    dialect: Dialect,
    file_rel: &'a str,
    records: &'a HashSet<String>,
    /// The qualified-name prefix, without the file component `qualify` adds for
    /// internal linkage.
    parts: Vec<String>,
    frames: Vec<Frame>,
    facts: FileFacts,
}

/// Stage 2: everything one file contributes, as plain data.
pub fn read_file(
    root: TsNode<'_>,
    source: &[u8],
    dialect: Dialect,
    file_rel: &str,
    records: &HashSet<String>,
) -> FileFacts {
    let mut reader = Reader {
        source,
        dialect,
        file_rel,
        records,
        parts: Vec::new(),
        frames: vec![Frame {
            parts: 0,
            owner: Owner::File,
            linkage: Linkage::External,
            access: "",
            member: false,
        }],
        facts: FileFacts {
            file_rel: file_rel.to_string(),
            declarations: Vec::new(),
            occurrences: Vec::new(),
            includes: Vec::new(),
        },
    };
    reader.walk(root);
    reader.facts
}

impl<'a> Reader<'a> {
    fn text(&self, node: TsNode<'_>) -> String {
        ts::text(node, self.source).into_owned()
    }

    fn owner(&self) -> Owner {
        self.frames
            .last()
            .map(|frame| frame.owner.clone())
            .unwrap_or(Owner::File)
    }

    fn access(&self) -> &'static str {
        self.frames.last().map(|frame| frame.access).unwrap_or("")
    }

    fn in_member_scope(&self) -> bool {
        self.frames.last().is_some_and(|frame| frame.member)
    }

    /// The linkage a name in the current scope has when nothing marks it.
    fn frame_linkage(&self) -> Linkage {
        self.frames
            .last()
            .map(|frame| frame.linkage)
            .unwrap_or(Linkage::External)
    }

    /// The linkage of a function or a data name.
    ///
    /// `static` is C's spelling of internal linkage — but only at file or
    /// namespace scope. Inside a class body the same keyword means "not bound to
    /// an instance" and says nothing about linkage, so treating a `static`
    /// member function as file-local would split its in-class declaration from
    /// its out-of-line definition into two nodes.
    ///
    /// In C++, and not in C, a namespace-scope `const` or `constexpr` variable
    /// has internal linkage by default. Getting that backwards is what would
    /// collapse two headers' `constexpr int kMax = 8;` into one node.
    ///
    /// `member` is passed in rather than read from the frame, because an
    /// out-of-line definition of a class member sits at namespace scope — and
    /// `const char *Base::DefaultLabel = "base";` must not come out internal
    /// while the in-class declaration of it came out external.
    fn data_linkage(&self, modifiers: Modifiers, is_data: bool, member: bool) -> Linkage {
        if self.frame_linkage() == Linkage::Internal {
            return Linkage::Internal;
        }
        if modifiers.is_extern {
            return Linkage::External;
        }
        if modifiers.is_static && !member {
            return Linkage::Internal;
        }
        if is_data && !member && self.dialect == Dialect::Cpp && modifiers.is_const {
            return Linkage::Internal;
        }
        Linkage::External
    }

    /// A name in the current scope, extended by the scope a declarator spelled.
    fn qualify_in(&self, extra: &[String], name: &str, linkage: Linkage) -> String {
        if extra.is_empty() {
            return qualify(self.file_rel, &self.parts, name, linkage);
        }
        let mut scope = self.parts.clone();
        scope.extend_from_slice(extra);
        qualify(self.file_rel, &scope, name, linkage)
    }

    fn push_frame(&mut self, components: Vec<String>, mut frame: Frame) {
        frame.parts = components.len();
        self.parts.extend(components);
        self.frames.push(frame);
    }

    fn pop_frame(&mut self) {
        if let Some(frame) = self.frames.pop() {
            self.parts.truncate(self.parts.len() - frame.parts);
        }
    }

    fn push_decl(&mut self, decl: FoundDecl) {
        self.facts.declarations.push(decl);
    }

    fn push_occurrence(
        &mut self,
        role: &'static str,
        name_node: TsNode<'_>,
        owner: &Owner,
    ) {
        self.facts.occurrences.push(FoundOccurrence {
            role,
            span: ts::span(name_node),
            owner: owner.clone(),
            name: self.text(name_node),
        });
    }

    /// An `annotation` for the name a type reduces to.
    fn record_annotation(&mut self, type_node: TsNode<'_>, owner: &Owner) {
        if let Some(name_node) = type_name_node(type_node) {
            self.push_occurrence("annotation", name_node, owner);
        }
    }

    // -- dispatch -----------------------------------------------------------

    fn walk(&mut self, container: TsNode<'_>) {
        for node in declarations(container) {
            self.dispatch(node);
        }
    }

    fn dispatch(&mut self, node: TsNode<'_>) {
        match node.kind() {
            "preproc_include" => self.on_include(node),
            "namespace_definition" => self.on_namespace(node),
            "function_definition" => self.on_function_definition(node),
            // `field_declaration` is a class member and `declaration` a file- or
            // namespace-scope one, but their shape is identical: an optional
            // `type`, several `declarator` fields, specifiers as unnamed
            // children. A constructor declaration is a `declaration` even inside
            // a class body, because it has no type of its own.
            "declaration" | "field_declaration" => self.on_declaration(node),
            "type_definition" => self.on_typedef(node),
            "struct_specifier" | "union_specifier" | "class_specifier" => {
                self.on_record(node);
            }
            "enum_specifier" => self.on_enum(node),
            "alias_declaration" => self.on_alias(node),
            "concept_definition" => self.on_concept(node),
            "using_declaration" => self.on_using(node),
            "access_specifier" => self.on_access_specifier(node),
            "enumerator" => self.on_enumerator(node),
            _ => {}
        }
    }

    // -- includes -----------------------------------------------------------

    /// `#include <stddef.h>`, `#include "engine.h"`, `#include MACRO_H`.
    ///
    /// A system path's text INCLUDES the angle brackets and a quoted one's the
    /// quotes; both are stripped here so `<engine.h>` and `"engine.h"` name the
    /// same header. A macro path — an `identifier` or a `call_expression` — is
    /// recorded verbatim and never resolved: expanding it is a preprocessor's
    /// job, and an unresolved include still becomes an EXTERNAL_SYMBOL.
    fn on_include(&mut self, node: TsNode<'_>) {
        let Some(path_node) = node.child_by_field_name("path") else {
            return;
        };
        let (path, style) = match path_node.kind() {
            "system_lib_string" => (
                self.text(path_node)
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string(),
                "system",
            ),
            // The quotes are separate anonymous children, so the content node is
            // taken rather than the literal trimmed; an empty `""` has none.
            "string_literal" => (
                ts::child_of_type(path_node, "string_content")
                    .map(|content| self.text(content))
                    .unwrap_or_default(),
                "quoted",
            ),
            _ => (self.text(path_node), "computed"),
        };
        if path.is_empty() {
            return;
        }
        self.facts.includes.push(FoundInclude {
            path,
            style,
            span: ts::span(node),
        });
    }

    // -- namespaces ---------------------------------------------------------

    /// `namespace fixture { … }`, `namespace a::b { … }`, `namespace { … }`.
    ///
    /// An anonymous namespace gives everything inside it INTERNAL linkage, which
    /// is C++'s replacement for file-scope `static` and the reason two files may
    /// each hold their own `fetch_one`.
    fn on_namespace(&mut self, node: TsNode<'_>) {
        let components = namespace_components(node, self.source);
        let linkage = if components.is_empty() {
            Linkage::Internal
        } else {
            self.frame_linkage()
        };
        let owner = self.owner();
        self.push_frame(
            components,
            Frame { parts: 0, owner, linkage, access: "", member: false },
        );
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(body);
        }
        self.pop_frame();
    }

    // -- records ------------------------------------------------------------

    fn on_access_specifier(&mut self, node: TsNode<'_>) {
        let text = self.text(node);
        let access = match text.trim() {
            "public" => "public",
            "protected" => "protected",
            _ => "private",
        };
        if let Some(frame) = self.frames.last_mut() {
            frame.access = access;
        }
    }

    /// `struct`, `union` and `class`, declared or defined.
    ///
    /// `name` is optional and `body` is optional. `struct S;` has a name and no
    /// body — a declaration — while `struct S { … };` has both, and that is the
    /// whole test for `defines`.
    ///
    /// An ANONYMOUS record inside another record's body is C's anonymous member:
    /// its fields are reached through the enclosing record, so no scope is
    /// pushed and its members land in the enclosing one. An anonymous record at
    /// file scope names nothing anything could refer to and is skipped whole.
    fn on_record(&mut self, node: TsNode<'_>) {
        let record_kind = match node.kind() {
            "union_specifier" => "union",
            "class_specifier" => "class",
            _ => "struct",
        };
        let body = node.child_by_field_name("body");

        let Some(name_node) = node.child_by_field_name("name") else {
            if let Some(body) = body
                && self.in_member_scope()
            {
                self.walk(body);
            }
            return;
        };

        let linkage = self.frame_linkage();
        let simple = self.text(name_node);
        let qname = self.qualify_in(&[], &simple, linkage);
        let modifiers = Modifiers::read(node, self.source);
        let owner = self.owner();
        let access = self.access();
        self.push_decl(FoundDecl {
            kind: NodeKind::Class,
            qualified_name: qname.clone(),
            name: simple.clone(),
            span: ts::span(node),
            name_span: ts::span(name_node),
            defines: body.is_some(),
            linkage,
            owner,
            record_kind,
            access,
            is_static: false,
            is_extern: false,
            is_virtual: false,
            is_template: modifiers.is_template,
            type_text: String::new(),
            signature: String::new(),
        });

        let record_owner = Owner::Decl {
            qualified_name: qname,
            kind: NodeKind::Class,
        };
        // `base_class_clause` has no field names at all: it interleaves an
        // `access_specifier` with the base type names.
        if let Some(bases) = ts::child_of_type(node, "base_class_clause") {
            for base in ts::named_children(bases) {
                if base.kind() == "access_specifier" {
                    continue;
                }
                let named = match base.kind() {
                    "qualified_identifier" => {
                        let (_, leaf) = split_qualified(base, self.source);
                        Some(bare_name(leaf))
                    }
                    _ => type_name_node(base),
                };
                if let Some(named) = named {
                    self.push_occurrence("base", named, &record_owner);
                }
            }
        }

        let Some(body) = body else { return };
        // `class` members default to private, `struct` and `union` to public.
        let member_access = if record_kind == "class" { "private" } else { "public" };
        self.push_frame(
            vec![simple],
            Frame {
                parts: 0,
                owner: record_owner,
                linkage,
                access: member_access,
                member: true,
            },
        );
        self.walk(body);
        self.pop_frame();
    }

    /// `enum region { … }` and `enum class Region { … }`.
    ///
    /// An UNSCOPED enum does not scope its enumerators: `REGION_EUROPE` is a name
    /// in the enclosing scope, and qualifying it under the enum would leave
    /// every use of it unbindable. `enum class` does scope them, so its members
    /// become `fixture::Region::Europe`. The two are one node kind in the
    /// grammar, told apart by an anonymous `class` or `struct` child.
    fn on_enum(&mut self, node: TsNode<'_>) {
        let body = node.child_by_field_name("body");
        let scoped = ts::children(node)
            .iter()
            .any(|child| matches!(child.kind(), "class" | "struct"));

        let linkage = self.frame_linkage();
        let access = self.access();
        let (owner, simple) = match node.child_by_field_name("name") {
            Some(name_node) => {
                let simple = self.text(name_node);
                let qname = self.qualify_in(&[], &simple, linkage);
                let file_owner = self.owner();
                self.push_decl(FoundDecl {
                    kind: NodeKind::Class,
                    qualified_name: qname.clone(),
                    name: simple.clone(),
                    span: ts::span(node),
                    name_span: ts::span(name_node),
                    defines: body.is_some(),
                    linkage,
                    owner: file_owner,
                    record_kind: "enum",
                    access,
                    is_static: false,
                    is_extern: false,
                    is_virtual: false,
                    is_template: false,
                    type_text: String::new(),
                    signature: String::new(),
                });
                (
                    Owner::Decl { qualified_name: qname, kind: NodeKind::Class },
                    simple,
                )
            }
            // `enum { A, B };` declares two constants and no type.
            None => (self.owner(), String::new()),
        };

        let Some(body) = body else { return };
        let components = if scoped && !simple.is_empty() {
            vec![simple]
        } else {
            Vec::new()
        };
        self.push_frame(
            components,
            Frame { parts: 0, owner, linkage, access, member: false },
        );
        self.walk(body);
        self.pop_frame();
    }

    fn on_enumerator(&mut self, node: TsNode<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let linkage = self.frame_linkage();
        let name = self.text(name_node);
        let qname = self.qualify_in(&[], &name, linkage);
        let owner = self.owner();
        let access = self.access();
        self.push_decl(FoundDecl {
            kind: NodeKind::Attribute,
            qualified_name: qname,
            name,
            span: ts::span(node),
            name_span: ts::span(name_node),
            defines: true,
            linkage,
            owner,
            record_kind: "enumerator",
            access,
            is_static: false,
            is_extern: false,
            is_virtual: false,
            is_template: false,
            type_text: String::new(),
            signature: String::new(),
        });
    }

    // -- typedefs, aliases, concepts ----------------------------------------

    /// `typedef struct engine engine_t;`, `typedef int (*handler)(void);`
    ///
    /// `declarator` is MULTIPLE — `typedef int myint, *myintp;` declares two —
    /// and the terminal is a `type_identifier` even when the chain runs through
    /// a `function_declarator`, because the name being introduced is a type.
    fn on_typedef(&mut self, node: TsNode<'_>) {
        let type_node = node.child_by_field_name("type");
        if let Some(type_node) = type_node {
            self.declare_inline_type(type_node);
        }
        let type_text = type_node.map(|n| self.text(n)).unwrap_or_default();
        let linkage = self.frame_linkage();

        let mut cursor = node.walk();
        let declarators: Vec<TsNode<'_>> =
            node.children_by_field_name("declarator", &mut cursor).collect();
        for declarator in declarators {
            let Some(found) = terminal_declarator(declarator) else {
                continue;
            };
            let leaf = bare_name(found.name);
            let name = self.text(leaf);
            if name.is_empty() {
                continue;
            }
            let qname = self.qualify_in(&[], &name, linkage);
            let owner = self.owner();
            let access = self.access();
            self.push_decl(FoundDecl {
                kind: NodeKind::TypeAlias,
                qualified_name: qname,
                name,
                span: ts::span(node),
                name_span: ts::span(leaf),
                defines: true,
                linkage,
                owner,
                record_kind: "",
                access,
                is_static: false,
                is_extern: false,
                is_virtual: false,
                is_template: false,
                type_text: type_text.clone(),
                signature: String::new(),
            });
        }
    }

    /// `using Identifier = long;`
    fn on_alias(&mut self, node: TsNode<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let type_node = node.child_by_field_name("type");
        let linkage = self.frame_linkage();
        let name = self.text(name_node);
        let qname = self.qualify_in(&[], &name, linkage);
        let owner = self.owner();
        let access = self.access();
        let is_template = Modifiers::read(node, self.source).is_template;
        let type_text = type_node.map(|n| self.text(n)).unwrap_or_default();
        self.push_decl(FoundDecl {
            kind: NodeKind::TypeAlias,
            qualified_name: qname.clone(),
            name,
            span: ts::span(node),
            name_span: ts::span(name_node),
            defines: true,
            linkage,
            owner,
            record_kind: "",
            access,
            is_static: false,
            is_extern: false,
            is_virtual: false,
            is_template,
            type_text,
            signature: String::new(),
        });
        if let Some(type_node) = type_node {
            let alias_owner = Owner::Decl {
                qualified_name: qname,
                kind: NodeKind::TypeAlias,
            };
            self.record_annotation(type_node, &alias_owner);
        }
    }

    /// `template <class T> concept Sizeable = …;`
    ///
    /// A TYPE_ALIAS: a concept is a named compile-time predicate over types, and
    /// of the fourteen node kinds that is the only honest fit.
    fn on_concept(&mut self, node: TsNode<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let linkage = self.frame_linkage();
        let name = self.text(name_node);
        let qname = self.qualify_in(&[], &name, linkage);
        let owner = self.owner();
        let access = self.access();
        self.push_decl(FoundDecl {
            kind: NodeKind::TypeAlias,
            qualified_name: qname,
            name,
            span: ts::span(node),
            name_span: ts::span(name_node),
            defines: true,
            linkage,
            owner,
            record_kind: "concept",
            access,
            is_static: false,
            is_extern: false,
            is_virtual: false,
            is_template: true,
            type_text: String::new(),
            signature: String::new(),
        });
    }

    /// `using n::C;` and `using namespace std;`
    ///
    /// No node: the declaration introduces no entity, it re-exports one. The
    /// name is recorded as a `read` so the reference is not lost.
    fn on_using(&mut self, node: TsNode<'_>) {
        let owner = self.owner();
        for child in ts::named_children(node) {
            let named = match child.kind() {
                "qualified_identifier" => {
                    let (_, leaf) = split_qualified(child, self.source);
                    Some(bare_name(leaf))
                }
                "identifier" | "type_identifier" | "namespace_identifier" => Some(child),
                _ => None,
            };
            if let Some(named) = named {
                self.push_occurrence("read", named, &owner);
            }
        }
    }

    // -- functions and data -------------------------------------------------

    /// A record definition written inline as a declaration's type.
    ///
    /// `struct engine { … } e;` at file scope and `class In { … };` inside
    /// another class both reach the walk as the `type` of a declaration rather
    /// than as a declaration of their own. Only a definition is declared here:
    /// `enum region region;` has a name and no body and is a use of the type.
    fn declare_inline_type(&mut self, type_node: TsNode<'_>) {
        if type_node.child_by_field_name("body").is_none() {
            return;
        }
        match type_node.kind() {
            "struct_specifier" | "union_specifier" | "class_specifier" => {
                self.on_record(type_node);
            }
            "enum_specifier" => self.on_enum(type_node),
            _ => {}
        }
    }

    fn on_function_definition(&mut self, node: TsNode<'_>) {
        let Some(declarator) = node.child_by_field_name("declarator") else {
            return;
        };
        let Some(found) = terminal_declarator(declarator) else {
            return;
        };
        let identity = self.callable_identity(node, &found);
        if identity.name.is_empty() {
            return;
        }
        let owner = Owner::Decl {
            qualified_name: identity.qualified_name.clone(),
            kind: identity.kind,
        };
        // `function_definition` is what the grammar produces for a body, for
        // `= default` and for `= delete`; every one of the three defines.
        self.declare_callable(node, &found, true, identity);
        if let Some(initializers) = ts::child_of_type(node, "field_initializer_list") {
            self.collect_initializers(initializers, &owner);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.collect_body(body, &owner);
        }
    }

    /// A `declaration` or a `field_declaration`, which may declare several
    /// things at once.
    ///
    /// `declarator` is MULTIPLE: `int *p, arr[10];` has two, and reading only
    /// the first would drop `arr`.
    fn on_declaration(&mut self, node: TsNode<'_>) {
        let type_node = node.child_by_field_name("type");
        if let Some(type_node) = type_node {
            self.declare_inline_type(type_node);
        }

        let mut cursor = node.walk();
        let declarators: Vec<TsNode<'_>> =
            node.children_by_field_name("declarator", &mut cursor).collect();
        for declarator in declarators {
            let Some(found) = terminal_declarator(declarator) else {
                continue;
            };
            if found.is_callable() {
                let identity = self.callable_identity(node, &found);
                if identity.name.is_empty() {
                    continue;
                }
                self.declare_callable(node, &found, false, identity);
                continue;
            }
            self.declare_data(node, &found, declarator, type_node);
        }
    }

    /// Everything about a callable that both its node and its body walk need.
    ///
    /// Derived once and passed on, because a call site inside the body has to
    /// enclose to exactly the node the declaration produced — and because
    /// deriving it involves normalizing every parameter type.
    fn callable_identity(&self, node: TsNode<'_>, found: &Declared<'_>) -> CallableId {
        let (extra, leaf) = split_qualified(found.name, self.source);
        let leaf = bare_name(leaf);
        let name = self.callable_name(found, leaf);

        // A qualified declarator names its own scope, and the last component of
        // it is reported as a `namespace_identifier` whether it is a namespace or
        // a class — the grammar cannot tell, because the class is usually
        // declared in another file. The workspace-wide record set is what
        // decides, which is why it has to be complete before this runs.
        let kind = match extra.last() {
            Some(scope) if self.records.contains(scope) => NodeKind::Method,
            Some(_) => NodeKind::Function,
            None if self.in_member_scope() => NodeKind::Method,
            None => NodeKind::Function,
        };

        let modifiers = Modifiers::read(node, self.source);
        let linkage = self.data_linkage(modifiers, false, kind == NodeKind::Method);
        // Overloads share a name and differ only in their parameters, so C++
        // folds the normalized list into the qualified name — without it two
        // overloads would hash to one id and one of them would vanish. C has no
        // overloading and must NOT do this, or every C function's name would grow
        // a parenthesis nobody querying the graph can predict.
        let signature = if self.dialect == Dialect::Cpp {
            found
                .signature_owner()
                .map(|signature_owner| self.signature_of(signature_owner))
                .unwrap_or_default()
        } else {
            String::new()
        };
        CallableId {
            kind,
            qualified_name: self.qualify_in(&extra, &format!("{name}{signature}"), linkage),
            name,
            // `operator_cast` carries no identifier of its own, so its name span
            // is the whole `operator int`; every other terminal has one.
            name_span: if found.name.kind() == "operator_cast" {
                ts::span(found.name)
            } else {
                ts::span(leaf)
            },
            linkage,
            signature,
            modifiers,
        }
    }

    /// The short name of a callable.
    ///
    /// `operator_cast` — `operator int() const` — has no identifier at all: its
    /// name IS the type it converts to, so the text up to the parameter list is
    /// taken and normalized.
    fn callable_name(&self, found: &Declared<'_>, leaf: TsNode<'_>) -> String {
        if found.name.kind() != "operator_cast" {
            return self.text(leaf);
        }
        let end = found
            .name
            .child_by_field_name("declarator")
            .map(|declarator| declarator.start_byte())
            .unwrap_or_else(|| found.name.end_byte());
        let start = found.name.start_byte();
        if end <= start {
            return self.text(found.name);
        }
        normalize_type(&String::from_utf8_lossy(&self.source[start..end]))
    }

    fn declare_callable(
        &mut self,
        node: TsNode<'_>,
        found: &Declared<'_>,
        defines: bool,
        identity: CallableId,
    ) {
        let return_type = node.child_by_field_name("type");
        let type_text = return_type.map(|n| self.text(n)).unwrap_or_default();
        let owner = self.owner();
        let access = self.access();
        self.push_decl(FoundDecl {
            kind: identity.kind,
            qualified_name: identity.qualified_name.clone(),
            name: identity.name,
            span: ts::span(node),
            name_span: identity.name_span,
            defines,
            linkage: identity.linkage,
            owner,
            record_kind: "",
            access,
            is_static: identity.modifiers.is_static,
            is_extern: identity.modifiers.is_extern,
            is_virtual: identity.modifiers.is_virtual,
            is_template: identity.modifiers.is_template,
            type_text,
            signature: identity.signature,
        });

        let callable_owner = Owner::Decl {
            qualified_name: identity.qualified_name.clone(),
            kind: identity.kind,
        };
        // A constructor, a destructor and a conversion operator have no return
        // type: `function_definition.type` is required in C and optional in C++.
        if let Some(return_type) = return_type {
            self.record_annotation(return_type, &callable_owner);
        }
        if let Some(signature_owner) = found.signature_owner() {
            self.collect_parameters(
                signature_owner,
                &identity.qualified_name,
                &callable_owner,
                defines,
            );
        }
    }

    /// PARAMETER nodes for a callable's parameters, plus their annotations.
    ///
    /// An unnamed parameter — `int engine_ping(const engine_t *);` — has nothing
    /// to name a node after and is skipped. `defines` is inherited from the
    /// callable so the definition's spelling of a parameter wins over the
    /// prototype's, which is the whole point of the ledger.
    fn collect_parameters(
        &mut self,
        signature_owner: TsNode<'_>,
        callable_qname: &str,
        callable_owner: &Owner,
        defines: bool,
    ) {
        let Some(list) = signature_owner.child_by_field_name("parameters") else {
            return;
        };
        for param in ts::named_children(list) {
            if !matches!(
                param.kind(),
                "parameter_declaration"
                    | "optional_parameter_declaration"
                    | "variadic_parameter_declaration"
            ) {
                continue;
            }
            let type_node = param.child_by_field_name("type");
            let name_node = param
                .child_by_field_name("declarator")
                .and_then(terminal_declarator)
                .map(|found| bare_name(found.name));
            let Some(name_node) = name_node else {
                if let Some(type_node) = type_node {
                    self.record_annotation(type_node, callable_owner);
                }
                continue;
            };
            let name = self.text(name_node);
            if name.is_empty() {
                continue;
            }
            let qname = format!("{callable_qname}{MEMBER_SEPARATOR}{name}");
            let type_text = type_node.map(|n| self.text(n)).unwrap_or_default();
            self.push_decl(FoundDecl {
                kind: NodeKind::Parameter,
                qualified_name: qname.clone(),
                name,
                span: ts::span(param),
                name_span: ts::span(name_node),
                defines,
                // A parameter is never visible outside the declaration that
                // names it, whatever the linkage of the callable.
                linkage: Linkage::Internal,
                owner: callable_owner.clone(),
                record_kind: "",
                access: "",
                is_static: false,
                is_extern: false,
                is_virtual: false,
                is_template: false,
                type_text,
                signature: String::new(),
            });
            if let Some(type_node) = type_node {
                let param_owner = Owner::Decl {
                    qualified_name: qname,
                    kind: NodeKind::Parameter,
                };
                self.record_annotation(type_node, &param_owner);
            }
        }
    }

    /// A variable at file or namespace scope, or a record member.
    ///
    /// A member becomes an ATTRIBUTE and everything else a VARIABLE. `extern`
    /// with no initializer declares without defining, which is what lets the
    /// definition elsewhere own the node.
    fn declare_data(
        &mut self,
        node: TsNode<'_>,
        found: &Declared<'_>,
        declarator: TsNode<'_>,
        type_node: Option<TsNode<'_>>,
    ) {
        // `auto [a, b] = …` binds several names through one declarator.
        if found.name.kind() == "structured_binding_declarator" {
            for bound in ts::named_children(found.name) {
                self.declare_one_data(node, bound, &[], declarator, type_node);
            }
            return;
        }
        // `const char *Base::DefaultLabel = "base";` defines a static data member
        // out of line, and it has to land on the node the in-class declaration
        // already produced — the same problem an out-of-line method has, solved
        // by the same workspace-wide record set.
        let (extra, leaf) = split_qualified(found.name, self.source);
        let leaf = bare_name(leaf);
        self.declare_one_data(node, leaf, &extra, declarator, type_node);
    }

    fn declare_one_data(
        &mut self,
        node: TsNode<'_>,
        name_node: TsNode<'_>,
        extra: &[String],
        declarator: TsNode<'_>,
        type_node: Option<TsNode<'_>>,
    ) {
        let name = self.text(name_node);
        if name.is_empty() {
            return;
        }
        let member = match extra.last() {
            Some(scope) => self.records.contains(scope),
            None => self.in_member_scope(),
        };
        let kind = if member { NodeKind::Attribute } else { NodeKind::Variable };
        let modifiers = Modifiers::read(node, self.source);
        let linkage = self.data_linkage(modifiers, true, member);
        // An initializer is an `init_declarator` at file scope and the
        // `default_value` field of a `field_declaration`; either one, or the
        // absence of `extern`, makes this the defining site.
        let initialized = declarator.kind() == "init_declarator"
            || node.child_by_field_name("default_value").is_some();
        let qname = self.qualify_in(extra, &name, linkage);
        let owner = self.owner();
        let access = self.access();
        let type_text = type_node.map(|n| self.text(n)).unwrap_or_default();
        self.push_decl(FoundDecl {
            kind,
            qualified_name: qname.clone(),
            name,
            span: ts::span(node),
            name_span: ts::span(name_node),
            defines: initialized || !modifiers.is_extern,
            linkage,
            owner,
            record_kind: "",
            access,
            is_static: modifiers.is_static,
            is_extern: modifiers.is_extern,
            is_virtual: false,
            is_template: modifiers.is_template,
            type_text,
            signature: String::new(),
        });
        if let Some(type_node) = type_node {
            let data_owner = Owner::Decl { qualified_name: qname, kind };
            self.record_annotation(type_node, &data_owner);
        }
    }

    /// The normalized parameter list of a callable, plus its trailing
    /// qualifiers.
    ///
    /// `(int) const` for `bool ping(int retries) const`. `(void)` normalizes to
    /// `()` because the two spell one signature, parameter names are dropped
    /// because they are not part of it, and a default value is dropped because a
    /// prototype carries it and the definition does not. The qualifiers are
    /// sorted so `const volatile` and `volatile const` cannot become two nodes;
    /// a ref-qualifier (`f() &&`) is deliberately ignored, so two overloads
    /// differing only in one share a node.
    fn signature_of(&self, signature_owner: TsNode<'_>) -> String {
        let mut params: Vec<String> = Vec::new();
        if let Some(list) = signature_owner.child_by_field_name("parameters") {
            for param in ts::named_children(list) {
                if !matches!(
                    param.kind(),
                    "parameter_declaration"
                        | "optional_parameter_declaration"
                        | "variadic_parameter_declaration"
                ) {
                    continue;
                }
                let text = self.parameter_type(param);
                if text.is_empty() || text == "void" {
                    continue;
                }
                params.push(text);
            }
        }
        let mut qualifiers: Vec<String> = ts::children(signature_owner)
            .into_iter()
            .filter(|child| child.kind() == "type_qualifier")
            .map(|child| self.text(child).trim().to_string())
            .filter(|text| !text.is_empty())
            .collect();
        qualifiers.sort();
        qualifiers.dedup();

        let mut out = format!("({})", params.join(","));
        for qualifier in qualifiers {
            out.push(' ');
            out.push_str(&qualifier);
        }
        out
    }

    /// One parameter's type, with its name and its default value removed.
    ///
    /// Sliced out of the source bytes rather than rebuilt from the children:
    /// node boundaries are always char boundaries, and reassembling
    /// `const engine_t *` from a declarator chain would mean re-deriving the
    /// grammar's punctuation.
    fn parameter_type(&self, param: TsNode<'_>) -> String {
        let start = param.start_byte();
        let end = param
            .child_by_field_name("default_value")
            .map(|value| value.start_byte())
            .unwrap_or_else(|| param.end_byte());
        if end <= start {
            return String::new();
        }
        let name = param
            .child_by_field_name("declarator")
            .and_then(terminal_declarator)
            .map(|found| bare_name(found.name))
            .map(|leaf| (leaf.start_byte(), leaf.end_byte()))
            .filter(|(name_start, name_end)| *name_start >= start && *name_end <= end);

        let mut bytes: Vec<u8> = Vec::with_capacity(end - start);
        match name {
            Some((name_start, name_end)) => {
                bytes.extend_from_slice(&self.source[start..name_start]);
                bytes.extend_from_slice(&self.source[name_end..end]);
            }
            None => bytes.extend_from_slice(&self.source[start..end]),
        }
        let raw = String::from_utf8_lossy(&bytes);
        // Cutting the default value off leaves the `=` behind.
        normalize_type(raw.trim_end().trim_end_matches('=').trim_end())
    }

    // -- use sites ----------------------------------------------------------

    /// `Engine::Engine(Region region) : region_(region)`.
    ///
    /// The member being initialized is written, and the expressions handed to it
    /// are ordinary values.
    fn collect_initializers(&mut self, list: TsNode<'_>, owner: &Owner) {
        for initializer in ts::named_children(list) {
            if initializer.kind() != "field_initializer" {
                continue;
            }
            if let Some(&first) = ts::named_children(initializer).first()
                && matches!(first.kind(), "field_identifier" | "identifier")
            {
                self.push_occurrence("write", first, owner);
            }
        }
        self.collect_body(list, owner);
    }

    /// Calls and member accesses inside a scope.
    ///
    /// Bare `identifier` reads are deliberately NOT recorded, as in the Go and
    /// Rust adapters: in C most of them are locals and parameters that no node
    /// could ever bind to, and recording them would bury the graph's real
    /// references under one occurrence per loop counter. The cost is that a read
    /// of a file-scope variable, which C spells with a bare identifier and
    /// nothing else, is invisible — a member access through `.` or `->` is not.
    fn collect_body(&mut self, scope: TsNode<'_>, owner: &Owner) {
        for node in descendants(scope) {
            match node.kind() {
                "call_expression" => {
                    if let Some(function) = node.child_by_field_name("function")
                        && let Some(named) = self.called_name(function)
                    {
                        self.push_occurrence("call", named, owner);
                    }
                }
                // `new Engine(1)` calls a constructor.
                "new_expression" => {
                    if let Some(type_node) = node.child_by_field_name("type")
                        && let Some(named) = type_name_node(type_node)
                    {
                        self.push_occurrence("call", named, owner);
                    }
                }
                "field_expression" => {
                    // A field in callee position was already recorded as a call;
                    // one site must not yield two occurrences.
                    if is_callee(node) {
                        continue;
                    }
                    if let Some(field) = node.child_by_field_name("field") {
                        let role = if is_write_target(node) { "write" } else { "read" };
                        self.push_occurrence(role, bare_name(field), owner);
                    }
                }
                _ => {}
            }
        }
    }

    /// The callee's name token; None when an expression result is called.
    fn called_name<'tree>(&self, function: TsNode<'tree>) -> Option<TsNode<'tree>> {
        match function.kind() {
            // engine_build(…)
            "identifier" => Some(function),
            // engine.ping() / handle->ping()
            "field_expression" => function.child_by_field_name("field").map(bare_name),
            // detail::label_for(…) / Engine::create(…)
            "qualified_identifier" => {
                let (_, leaf) = split_qualified(function, self.source);
                Some(bare_name(leaf))
            }
            "template_function" | "template_method" => Some(bare_name(function)),
            // (*fp)(x), handlers[0](x): nothing names the callee.
            _ => None,
        }
    }
}

/// Stage 3: the facts of one file, written into the graph.
///
/// Holds no mutable state, so one emitter serves every file of a root and the
/// graph stays a parameter rather than a borrowed field.
pub struct CFamilyEmitter<'a> {
    py: Python<'a>,
    project_name: &'a str,
    /// Filled by [`FileFacts::survey`] over EVERY file before the first call to
    /// [`CFamilyEmitter::emit`].
    ledger: &'a DeclLedger,
    /// From `headers::include_search_dirs`.
    search_dirs: &'a [String],
    /// Project-relative paths of every file the adapter collected. An include
    /// resolves only against these: resolving against the filesystem would bind
    /// `/usr/include/stdio.h`, which has no FILE node to point at.
    known_files: &'a HashSet<String>,
}

impl<'a> CFamilyEmitter<'a> {
    pub fn new(
        py: Python<'a>,
        project_name: &'a str,
        ledger: &'a DeclLedger,
        search_dirs: &'a [String],
        known_files: &'a HashSet<String>,
    ) -> Self {
        Self { py, project_name, ledger, search_dirs, known_files }
    }

    /// Nodes, structural edges and occurrences for one file.
    ///
    /// `file_id` must be the id the adapter gave the FILE node, which by family
    /// convention is `node_id(project, file_rel, "file")`.
    pub fn emit(
        &self,
        graph: &mut Graph,
        facts: &FileFacts,
        file_id: &str,
    ) -> PyResult<Vec<OccurrenceRef>> {
        for decl in &facts.declarations {
            self.emit_decl(graph, facts, file_id, decl)?;
        }
        for include in &facts.includes {
            self.emit_include(graph, facts, file_id, include)?;
        }
        Ok(facts
            .occurrences
            .iter()
            .map(|occurrence| {
                let enclosing_id = self.owner_id(&occurrence.owner, file_id);
                OccurrenceRef {
                    role: occurrence.role.to_string(),
                    line: occurrence.span.start_line,
                    col: occurrence.span.start_col,
                    enclosing_id,
                    span: occurrence.span,
                }
            })
            .collect())
    }

    /// The node an Owner names.
    fn owner_id(&self, owner: &Owner, file_id: &str) -> String {
        match owner {
            Owner::File => file_id.to_string(),
            Owner::Decl { qualified_name, kind } => {
                node_id(self.project_name, qualified_name, kind.as_str())
            }
        }
    }

    fn emit_decl(
        &self,
        graph: &mut Graph,
        facts: &FileFacts,
        file_id: &str,
        decl: &FoundDecl,
    ) -> PyResult<()> {
        let key: DeclKey = (decl.kind, decl.qualified_name.clone());
        let site = DeclSite {
            file_rel: facts.file_rel.clone(),
            span: decl.span,
            name_span: decl.name_span,
            defines: decl.defines,
        };
        let id = node_id(self.project_name, &decl.qualified_name, decl.kind.as_str());

        // Only the site the ledger picked writes the node, so its span is the
        // definition's rather than whichever file happened to be walked first.
        if self.ledger.owns(&key, &site) && !graph.has_node(&id) {
            let declared_in = self.ledger.declared_elsewhere(&key);
            let metadata = self.metadata_for(decl, &declared_in)?;
            let node = Node {
                id: id.clone(),
                kind: decl.kind,
                qualified_name: decl.qualified_name.clone(),
                name: decl.name.clone(),
                file_path: Some(facts.file_rel.clone()),
                span: Some(decl.span),
                metadata: metadata.unbind(),
            };
            graph.insert_node(id.clone(), Py::new(self.py, node)?);
        }

        // DECLARES from EVERY declaring file, whether or not the node was
        // written here: a prototype in a header is a real declaration, and
        // "which files declare engine_build" has two answers.
        let owner_id = self.owner_id(&decl.owner, file_id);
        if decl.kind != NodeKind::Parameter && owner_id != file_id {
            push_relation(
                self.py,
                graph,
                file_id.to_string(),
                id.clone(),
                RelationKind::Declares,
            )?;
        }
        push_relation(self.py, graph, owner_id, id, RelationKind::Declares)
    }

    fn metadata_for(
        &self,
        decl: &FoundDecl,
        declared_in: &[String],
    ) -> PyResult<Bound<'a, PyDict>> {
        let metadata = PyDict::new(self.py);
        // SpanIndex maps a definition's position back to the node through the
        // NAME span, not the full extent.
        metadata.set_item("name_span", decl.name_span)?;

        match decl.kind {
            NodeKind::Function | NodeKind::Method => {
                metadata.set_item("linkage", decl.linkage.as_str())?;
                metadata.set_item("is_definition", decl.defines)?;
                metadata.set_item("is_static", decl.is_static)?;
                metadata.set_item("is_virtual", decl.is_virtual)?;
                metadata.set_item("is_template", decl.is_template)?;
                metadata.set_item("signature", &decl.signature)?;
                metadata.set_item("return_type", &decl.type_text)?;
                metadata.set_item("access", decl.access)?;
            }
            NodeKind::Variable => {
                metadata.set_item("linkage", decl.linkage.as_str())?;
                metadata.set_item("is_definition", decl.defines)?;
                metadata.set_item("is_static", decl.is_static)?;
                metadata.set_item("is_extern", decl.is_extern)?;
                metadata.set_item("annotation", &decl.type_text)?;
            }
            NodeKind::Attribute => {
                metadata.set_item("access", decl.access)?;
                metadata.set_item("is_static", decl.is_static)?;
                metadata.set_item("annotation", &decl.type_text)?;
                metadata.set_item("is_enumerator", decl.record_kind == "enumerator")?;
            }
            NodeKind::Class => {
                metadata.set_item("record_kind", decl.record_kind)?;
                metadata.set_item("linkage", decl.linkage.as_str())?;
                metadata.set_item("is_definition", decl.defines)?;
                metadata.set_item("is_template", decl.is_template)?;
                metadata.set_item("access", decl.access)?;
            }
            NodeKind::TypeAlias => {
                metadata.set_item("underlying", &decl.type_text)?;
                metadata.set_item("is_concept", decl.record_kind == "concept")?;
                metadata.set_item("is_template", decl.is_template)?;
            }
            _ => {
                metadata.set_item("annotation", &decl.type_text)?;
            }
        }
        // A PARAMETER has exactly one declaring site per spelling of its name,
        // so the key would always be an empty list.
        if decl.kind != NodeKind::Parameter {
            metadata.set_item("declared_in", PyList::new(self.py, declared_in)?)?;
        }
        Ok(metadata)
    }

    /// An IMPORT node for one `#include`, bound to the header it names.
    ///
    /// The header's FILE node id is COMPUTED rather than looked up. Ids are
    /// deterministic and the adapter creates a FILE node for every path in
    /// `known_files`, so the target exists whatever order the files are emitted
    /// in — which is what keeps this from needing the deferred second pass the
    /// Go and Rust adapters use for internal imports.
    fn emit_include(
        &self,
        graph: &mut Graph,
        facts: &FileFacts,
        file_id: &str,
        include: &FoundInclude,
    ) -> PyResult<()> {
        let resolved = if include.style == "computed" {
            None
        } else {
            resolve_include(
                &include.path,
                include.style == "quoted",
                &facts.file_rel,
                self.search_dirs,
                self.known_files,
            )
        };
        // `stdlib` is not claimed for an unresolved `<…>`: `<stdio.h>` is the
        // standard library and `<curl/curl.h>` is a third-party package, and
        // nothing in the source tells them apart. `include_style` keeps the
        // distinction that IS available.
        let origin = if resolved.is_some() { "internal" } else { "unknown" };

        // The including file is part of the name: an IMPORT node records that
        // THIS file includes the header, so twelve files including `engine.h`
        // are twelve import sites and one header.
        let import_qname =
            format!("{}{SCOPE_SEPARATOR}{}", facts.file_rel, include.path);
        let import_id =
            node_id(self.project_name, &import_qname, NodeKind::Import.as_str());
        if !graph.has_node(&import_id) {
            let metadata = PyDict::new(self.py);
            metadata.set_item("origin", origin)?;
            metadata.set_item("include_path", &include.path)?;
            metadata.set_item("include_style", include.style)?;
            metadata.set_item("resolved_path", resolved.as_deref())?;
            let node = Node {
                id: import_id.clone(),
                kind: NodeKind::Import,
                qualified_name: import_qname,
                name: include.path.clone(),
                file_path: Some(facts.file_rel.clone()),
                span: Some(include.span),
                metadata: metadata.unbind(),
            };
            graph.insert_node(import_id.clone(), Py::new(self.py, node)?);
        }
        push_relation(
            self.py,
            graph,
            file_id.to_string(),
            import_id.clone(),
            RelationKind::Declares,
        )?;

        let target_id = match &resolved {
            Some(path) => node_id(self.project_name, path, NodeKind::File.as_str()),
            None => self.ensure_external_symbol(graph, &include.path, origin)?,
        };
        push_relation(
            self.py,
            graph,
            file_id.to_string(),
            target_id.clone(),
            RelationKind::Imports,
        )?;
        push_relation(self.py, graph, import_id, target_id, RelationKind::ResolvesTo)
    }

    fn ensure_external_symbol(
        &self,
        graph: &mut Graph,
        qname: &str,
        origin: &str,
    ) -> PyResult<String> {
        let sym_id = node_id(self.project_name, qname, NodeKind::ExternalSymbol.as_str());
        if !graph.has_node(&sym_id) {
            let metadata = PyDict::new(self.py);
            metadata.set_item("origin", origin)?;
            let node = Node {
                id: sym_id.clone(),
                kind: NodeKind::ExternalSymbol,
                qualified_name: qname.to_string(),
                name: qname.rsplit('/').next().unwrap_or(qname).to_string(),
                file_path: None,
                span: None,
                metadata: metadata.unbind(),
            };
            graph.insert_node(sym_id.clone(), Py::new(self.py, node)?);
        }
        Ok(sym_id)
    }
}

fn push_relation(
    py: Python<'_>,
    graph: &mut Graph,
    source_id: String,
    target_id: String,
    kind: RelationKind,
) -> PyResult<()> {
    let relation = Relation {
        source_id,
        target_id,
        kind,
        metadata: PyDict::new(py).unbind(),
    };
    graph.push_relation(Py::new(py, relation)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The normalization a C++ overload's id depends on: two spellings of one
    /// parameter type have to reduce to one string, or a header and its source
    /// file produce two nodes for one function.
    #[test]
    fn normalize_type_folds_the_spellings_of_one_type() {
        assert_eq!(normalize_type("const char *"), "const char*");
        assert_eq!(normalize_type("const char*"), "const char*");
        assert_eq!(normalize_type("  const   char  * "), "const char*");
        assert_eq!(normalize_type("T && ..."), "T&&...");
        // Whitespace between two words is meaning, not formatting.
        assert_eq!(normalize_type("unsigned long long"), "unsigned long long");
        assert_eq!(normalize_type("operator  int"), "operator int");
        assert_eq!(normalize_type(""), "");
    }

    #[test]
    fn normalize_type_is_a_fixed_point() {
        for text in ["int", "const char*", "unsigned long", "T&&...", "std::string&"] {
            assert_eq!(normalize_type(text), text, "{text} is not stable");
        }
    }
}
