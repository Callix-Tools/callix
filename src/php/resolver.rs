//! The PHP symbol resolver: a symbol table built from the sources themselves.
//!
//! Every other language callix analyses has a type checker behind it — `ty`
//! linked in, typescript-go, `go/types`, `rust-analyzer`. PHP has none: there is
//! nothing to link and nothing widely installed to shell out to. graphlens drove
//! `intelephense --stdio`, a Node.js language server, which is the reason that
//! package was never published; callix will not depend on Node.
//!
//! So this is **a symbol table, not a type checker**, and it reports
//! `ResolverStatus::Degraded` for exactly that reason. What it does answer:
//!
//! * a name, through the file's `use` aliases, then the enclosing namespace,
//!   then the global namespace, then the file PSR-4 would load it from;
//! * `$this->m()`, `self::m()`, `static::m()` and `parent::m()`, and the keywords
//!   themselves wherever they are written (a type, `new`, `instanceof`, `::`):
//!   `self` and `static` are the class they are written in (no late static
//!   binding), `parent` is its first base class;
//! * `$x = new Foo(); $x->m()`, and the same receiver from a typed parameter, a
//!   typed property, a promoted constructor property and a `catch` variable;
//! * a member inherited from a base class, an interface or a trait.
//!
//! What is out of reach, and answers nothing rather than guessing: a dynamic
//! class name (`$cls::m()`), `__call` and the other magic methods, a variable
//! whose type comes from a function's return value or a container's `get()`, a
//! union-typed receiver, and anything at all in `vendor/` — which is
//! deliberately not collected, so a call into a dependency resolves to an
//! EXTERNAL_SYMBOL named after the class rather than to its source.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::py::prelude::*;
use crate::py::derive::{gen_stub_pyclass, gen_stub_pymethods};
use tree_sitter::Node as TsNode;

use crate::python::ResolvedRef;
use crate::status::ResolverStatus;
use crate::ts;

use super::deps::{classify_import, dependencies, internal_prefixes, stdlib_names};
use super::detector::{php_roots, psr4_roots};
use super::visitor::has_php_statements;

/// A declaration's site: the document and the 1-based position of its NAME
/// token, which is what `SpanIndex::lookup_name` binds against.
#[derive(Clone, Copy)]
struct Site {
    doc: u32,
    line: u32,
    col: u32,
}

/// Which table a member use-site means.
///
/// The syntax says it — `$x->m()` and `X::m()` are methods, `$x->p` and `X::$p`
/// properties, `X::C` a constant — and keeping them apart matters because PHP
/// allows one class to have a method and a property of the same name.
#[derive(Clone, Copy)]
enum Slot {
    Method,
    /// A method named through its class, `A::m()`: undeclared, it goes to
    /// `__callStatic` before `__call`.
    StaticMethod,
    Property,
    Constant,
}

/// How a name is looked up when more than one table could answer.
#[derive(Clone, Copy)]
enum NameKind {
    /// A type, a base class, `new`, an attribute.
    Class,
    /// The callee of `f(…)`.
    Function,
    /// A bare identifier read: a constant, or a class named in passing.
    Value,
}

/// A class the workspace declares, or one it only mentions.
#[derive(Clone, PartialEq)]
enum ClassRef {
    /// The key into the class tables.
    Declared(String),
    /// A fully-qualified name that nothing in the workspace declares.
    Foreign(String),
}

/// What one use-site refers to, computed during `prepare`.
enum Answer {
    /// A declaration in the workspace, given as the position its name token
    /// starts at for the adapter's `SpanIndex` to bind.
    Internal(Site),
    /// A name outside it: vendor, PHP itself, or a symbol the index could name
    /// but not place.
    External { name: String, origin: &'static str },
}

/// A class, interface, trait or enum the workspace declares.
struct ClassEntry {
    /// The declared spelling. Table keys are lowercased, and an external member
    /// of the class has to read `App\Model\User\save`, not the lowercased key.
    fqn: String,
    site: Site,
}

/// One file's `use` statements, by what they import.
#[derive(Default)]
struct Uses {
    classes: HashMap<String, String>,
    functions: HashMap<String, String>,
    constants: HashMap<String, String>,
}

/// A name that can only be resolved once every file has been indexed: a base
/// class or a property's type is routinely declared in a file read later.
struct Pending {
    /// The owning class, as a table key.
    class: String,
    /// What the name is: a property's type, or a parent link of some kind.
    role: PendingRole,
    name: String,
    doc: u32,
    namespace: String,
}

/// Which kind of parent link a name is.
#[derive(Clone, Copy)]
enum ParentKind {
    Base,
    Trait,
    Interface,
}

/// What a pending name stands for.
enum PendingRole {
    /// The type of this property (a method return type is `()name`).
    Property(String),
    /// A base class, a trait or an interface of the owning class.
    Parent(ParentKind),
}

/// A class's parents, by kind, each in declared order. PHP searches them in a
/// fixed order (traits, then base, then interfaces), so the kind matters.
#[derive(Default)]
struct Parents {
    traits: Vec<String>,
    /// A Vec only because half-written code can carry more than one `extends`.
    bases: Vec<String>,
    interfaces: Vec<String>,
}

/// The fully-qualified names a written name may mean, most specific first.
struct Candidates {
    names: Vec<String>,
    /// Whether the first candidate is authoritative — the name was absolute, or
    /// the file's `use` map named its target. Otherwise the tail of the list is
    /// the global fallback PHP itself applies to functions and constants.
    bound: bool,
}

/// The final `name` of a possibly qualified reference, which is where the
/// visitor recorded the occurrence: `App\Model\User` → `User`.
fn last_name<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    if node.kind() == "name" {
        return Some(node);
    }
    ts::children(node).into_iter().rev().find(|c| c.kind() == "name")
}

/// Built-in functions that call the callable they are given. A callable named by
/// a string or an array is only a call when the function receiving it makes one,
/// which is why the list is closed: guessing for every function that takes a
/// string would bind half the strings of a program.
pub(super) const CALLBACK_FUNCTIONS: [&str; 22] = [
    "array_map", "array_filter", "array_reduce", "array_walk", "array_walk_recursive",
    "usort", "uasort", "uksort", "array_udiff", "array_uintersect", "call_user_func",
    "call_user_func_array", "is_callable", "iterator_apply", "register_shutdown_function",
    "set_error_handler", "set_exception_handler", "spl_autoload_register", "preg_replace_callback",
    "array_diff_ukey", "forward_static_call", "forward_static_call_array",
];

/// The callable a call to a callback-taking built-in names, if any: the string
/// node of `'name'`, or of the method in `[$object, 'method']`, with the
/// expression that names the class in the second case.
pub(super) struct CallableLiteral<'tree> {
    pub position: TsNode<'tree>,
    pub text: String,
    pub class: Option<CallableClass<'tree>>,
}

/// How a callable names the class its method belongs to.
pub(super) enum CallableClass<'tree> {
    /// `[$object, 'm']`, `[Foo::class, 'm']`: an expression.
    Expression(TsNode<'tree>),
    /// `'Foo::m'`: a class name written in the string.
    Named(String),
}

/// The string literals and `[class, 'method']` pairs among a call's arguments.
pub(super) fn callable_literals<'tree>(
    call: TsNode<'tree>,
    source: &[u8],
) -> Vec<CallableLiteral<'tree>> {
    let Some(function) = call.child_by_field_name("function") else {
        return Vec::new();
    };
    if !matches!(function.kind(), "name" | "qualified_name" | "relative_name") {
        return Vec::new();
    }
    let callee = ts::text(function, source).to_lowercase();
    let callee = callee.rsplit('\\').next().unwrap_or(&callee);
    if !CALLBACK_FUNCTIONS.contains(&callee) {
        return Vec::new();
    }
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    callable_arguments(arguments, source)
        .into_iter()
        .filter_map(|(_, value)| match value {
            CallableValue::Literal(literal) => Some(literal),
            CallableValue::Named(_) => None,
        })
        .collect()
}

/// Whether `call` is a call of a built-in that calls its callable arguments.
pub(super) fn is_callback_call(call: TsNode<'_>, source: &[u8]) -> bool {
    let Some(function) = call.child_by_field_name("function") else {
        return false;
    };
    if !matches!(function.kind(), "name" | "qualified_name" | "relative_name") {
        return false;
    }
    let callee = ts::text(function, source).to_lowercase();
    CALLBACK_FUNCTIONS.contains(&callee.rsplit('\\').next().unwrap_or(&callee))
}

/// A callable written out in an expression.
pub(super) enum CallableValue<'tree> {
    /// A string or a `[class, 'method']` pair, which only the resolver can place.
    Literal(CallableLiteral<'tree>),
    /// `g(...)`, `$o->m(...)`: the name the resolver already answers.
    Named(TsNode<'tree>),
}

impl<'tree> CallableValue<'tree> {
    /// The place the visitor records the occurrence at.
    pub(super) fn position(&self) -> TsNode<'tree> {
        match self {
            CallableValue::Literal(literal) => literal.position,
            CallableValue::Named(node) => *node,
        }
    }
}

/// The callable `value` writes out, if it does: a string naming a function or
/// `'Class::method'`, a `[class, 'method']` pair, a first-class callable
/// (`g(...)`), or `Closure::fromCallable` of one of those.
pub(super) fn callable_value<'tree>(value: TsNode<'tree>, source: &[u8]) -> Option<CallableValue<'tree>> {
    match value.kind() {
        "string" | "encapsed_string" => {
            let text = string_literal(value, source)?;
            if !text.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '\\' | ':')) {
                return None;
            }
            match text.split_once("::") {
                Some((class, method)) if !class.is_empty() && !method.is_empty() && !method.contains(':') => {
                    Some(CallableValue::Literal(CallableLiteral {
                        position: value,
                        text: method.to_string(),
                        class: Some(CallableClass::Named(class.to_string())),
                    }))
                }
                Some(_) => None,
                None => Some(CallableValue::Literal(CallableLiteral { position: value, text, class: None })),
            }
        }
        "array_creation_expression" => {
            let elements: Vec<TsNode<'tree>> = ts::children(value)
                .into_iter()
                .filter(|c| c.kind() == "array_element_initializer")
                .collect();
            let [first, second] = elements.as_slice() else {
                return None;
            };
            let class = ts::children(*first).into_iter().find(|c| c.is_named())?;
            let method = ts::children(*second).into_iter().find(|c| c.is_named())?;
            if !matches!(method.kind(), "string" | "encapsed_string") {
                return None;
            }
            let text = string_literal(method, source)?;
            Some(CallableValue::Literal(CallableLiteral {
                position: method,
                text,
                class: Some(CallableClass::Expression(class)),
            }))
        }
        "parenthesized_expression" => {
            callable_value(ts::named_children(value).into_iter().next()?, source)
        }
        "function_call_expression" | "member_call_expression" | "nullsafe_member_call_expression"
        | "scoped_call_expression" => {
            if is_first_class_callable(value) {
                let name = if value.kind() == "function_call_expression" {
                    last_name(value.child_by_field_name("function")?)?
                } else {
                    value.child_by_field_name("name").filter(|n| n.kind() == "name")?
                };
                return Some(CallableValue::Named(name));
            }
            // `Closure::fromCallable(x)` is `x`.
            if value.kind() == "scoped_call_expression"
                && let Some(scope) = value.child_by_field_name("scope")
                && ts::text(scope, source).trim_start_matches('\\').eq_ignore_ascii_case("closure")
                && value
                    .child_by_field_name("name")
                    .is_some_and(|n| ts::text(n, source).eq_ignore_ascii_case("fromCallable"))
                && let Some(arguments) = value.child_by_field_name("arguments")
                && let Some(argument) = ts::children(arguments).into_iter().find(|c| c.kind() == "argument")
                && let Some(inner) = ts::children(argument).into_iter().rev().find(|c| c.is_named())
            {
                return callable_value(inner, source);
            }
            None
        }
        _ => None,
    }
}

/// `g(...)`, `$o->m(...)`, `A::m(...)`: a callable made from a function, which
/// is not a call of it.
pub(super) fn is_first_class_callable(call: TsNode<'_>) -> bool {
    call.child_by_field_name("arguments")
        .is_some_and(|arguments| ts::children(arguments).iter().any(|c| c.kind() == "variadic_placeholder"))
}

/// The arguments of a call that are a callable written out, each with its slot -
/// the position, or `=name` for a named argument - the way a call's arguments are
/// bound to the parameters of what it calls.
pub(super) fn callable_arguments<'tree>(
    arguments: TsNode<'tree>,
    source: &[u8],
) -> Vec<(String, CallableValue<'tree>)> {
    let mut out = Vec::new();
    for (index, argument) in ts::children(arguments)
        .into_iter()
        .filter(|c| c.kind() == "argument")
        .enumerate()
    {
        let slot = match argument.child_by_field_name("name") {
            Some(label) => format!("={}", ts::text(label, source)),
            None => index.to_string(),
        };
        if let Some(value) = ts::children(argument).into_iter().rev().find(|c| c.is_named())
            && let Some(callable) = callable_value(value, source)
        {
            out.push((slot, callable));
        }
    }
    out
}

/// The text of a string literal with no interpolation, unescaped as far as a
/// name needs: `'App\\f'` is `App\f`.
fn string_literal(node: TsNode<'_>, source: &[u8]) -> Option<String> {
    let raw = ts::text(node, source);
    let inner = raw.get(1..raw.len().checked_sub(1)?)?;
    if inner.is_empty() || inner.contains('$') || inner.contains('{') {
        return None;
    }
    Some(inner.replace("\\\\", "\\").trim_start_matches('\\').to_string())
}

/// The bare identifier behind a variable: `$user` → `user`.
///
/// `by_ref` wraps the variable in `&$user`, which a parameter accepts.
fn variable_identifier<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    let variable = if node.kind() == "by_ref" {
        ts::child_of_type(node, "variable_name")?
    } else {
        node
    };
    ts::child_of_type(variable, "name")
}

/// Class-name leaves of a type or a heritage clause.
///
/// The node is kept whole rather than reduced to its last segment: its text
/// carries the namespace the resolution needs, while `last_name` gives the
/// position the visitor recorded. `type` is a supertype — a nullable, union,
/// intersection or DNF type is a different node kind each time — so one
/// recursive walk covers every composite shape.
fn collect_class_refs<'tree>(node: TsNode<'tree>, out: &mut Vec<TsNode<'tree>>) {
    match node.kind() {
        "name" | "qualified_name" | "relative_name" => out.push(node),
        // `int`, `string`, `never`: a built-in names nothing to resolve.
        "primitive_type" | "bottom_type" => {}
        _ => {
            for child in ts::children(node) {
                collect_class_refs(child, out);
            }
        }
    }
}

fn class_refs<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let mut out = Vec::new();
    collect_class_refs(node, &mut out);
    out
}

/// A name inside a namespace.
fn qualify(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{namespace}\\{name}")
    }
}

/// What a `use` statement imports: `class`, `function` or `const`.
///
/// The `type:` field holds an anonymous token, which makes this the only place
/// the distinction is visible.
fn use_kind(node: TsNode<'_>) -> &'static str {
    match node.child_by_field_name("type").map(|n| n.kind()) {
        Some("function") => "function",
        Some("const") => "const",
        _ => "class",
    }
}

/// One import of a `use` statement, group clauses already joined to their prefix.
pub(super) struct UseClause<'tree> {
    /// The fully-qualified name imported, without a leading `\`.
    pub target: String,
    /// The final `name` token of the ORIGINAL path (never the alias): this is
    /// where the visitor records the `import` occurrence and where the answer is keyed.
    pub last: TsNode<'tree>,
    /// `class`, `function` or `const`.
    pub kind: &'static str,
}

/// Every clause of a `namespace_use_declaration`, the group form included.
pub(super) fn use_clauses<'tree>(decl: TsNode<'tree>, source: &[u8]) -> Vec<UseClause<'tree>> {
    let declared = use_kind(decl);
    let (prefix, clauses) = match decl.child_by_field_name("body") {
        Some(group) => (
            ts::child_of_type(decl, "namespace_name")
                .map(|n| ts::text(n, source).into_owned())
                .unwrap_or_default(),
            ts::children(group),
        ),
        None => (String::new(), ts::children(decl)),
    };
    let mut out = Vec::new();
    for clause in clauses {
        if clause.kind() != "namespace_use_clause" {
            continue;
        }
        let Some(path_node) = ts::child_of_types(clause, &["qualified_name", "name"]) else {
            continue;
        };
        let Some(last) = last_name(path_node) else {
            continue;
        };
        let path = ts::text(path_node, source);
        let path = path.trim_matches('\\');
        let target = if prefix.is_empty() {
            path.to_string()
        } else {
            format!("{}\\{path}", prefix.trim_matches('\\'))
        };
        if target.is_empty() {
            continue;
        }
        let kind = match use_kind(clause) {
            "class" => declared,
            own => own,
        };
        out.push(UseClause { target, last, kind });
    }
    out
}

/// The namespace a `namespace` statement or block names.
fn namespace_of(node: TsNode<'_>, source: &[u8]) -> String {
    node.child_by_field_name("name")
        .map(|n| ts::text(n, source).trim_matches('\\').to_string())
        .unwrap_or_default()
}

/// The declaration walk's state.
struct DeclScope {
    namespace: String,
    /// The declared FQN of the class whose body is being walked.
    class: Option<String>,
    /// Mirrors the top of the visitor's `scope_stack` (never lowercased): the
    /// prefix the visitor gives an anonymous class's name. Namespaces, named
    /// classes, methods and functions extend it; closures do not.
    prefix: String,
}

/// The use-site walk's state.
#[derive(Default)]
struct UseScope {
    namespace: String,
    /// The enclosing class — `$this`, `self` and `static`.
    class: Option<ClassRef>,
    /// Its first base class — `parent`.
    parent: Option<ClassRef>,
    /// Local variable name → the class it holds.
    vars: HashMap<String, ClassRef>,
    /// Local variable name → the one string it is assigned, `None` once it is
    /// assigned anything else: the method name of `$this->$m()`.
    strings: HashMap<String, Option<String>>,
    /// Local variable name → the one class name it is assigned (`Foo::class`),
    /// `None` once it is assigned anything else: `new $class()`, `$class::m()`.
    class_strings: HashMap<String, Option<ClassRef>>,
}

/// Which class a `self`, `static` or `parent` keyword stands for.
#[derive(Clone, Copy)]
enum Relative {
    /// `self` and `static`: the class the keyword is written in.
    Enclosing,
    /// `parent`: its first base class.
    Parent,
}

/// The keyword a written name is, matched the way PHP does: ASCII case-insensitively.
fn relative_keyword(text: &str) -> Option<Relative> {
    if text.eq_ignore_ascii_case("self") || text.eq_ignore_ascii_case("static") {
        Some(Relative::Enclosing)
    } else if text.eq_ignore_ascii_case("parent") {
        Some(Relative::Parent)
    } else {
        None
    }
}

/// The class a keyword names in this scope, or None when the scope has none.
fn relative_class(scope: &UseScope, which: Relative) -> Option<ClassRef> {
    match which {
        Relative::Enclosing => scope.class.clone(),
        Relative::Parent => scope.parent.clone(),
    }
}

type MemberKey = (String, String);
type MemberTable = HashMap<MemberKey, Site>;
type Answers = HashMap<(u32, u32), Answer>;

#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct PhpResolver {
    root: Option<PathBuf>,
    status: ResolverStatus,
    /// Project-relative document paths and the reverse lookup over them.
    docs: Vec<String>,
    doc_index: HashMap<String, u32>,
    /// Per document: use-site position → what it refers to.
    answers: Vec<Answers>,
    /// Per document: its `use` statements.
    uses: Vec<Uses>,
    // -- the symbol table ---------------------------------------------------
    /// Classes, interfaces, traits and enums. PHP compares class, function and
    /// method names case-insensitively, so those keys are lowercased; property
    /// and constant names it compares exactly, and they are not.
    classes: HashMap<String, ClassEntry>,
    functions: HashMap<String, Site>,
    constants: HashMap<String, Site>,
    methods: MemberTable,
    properties: MemberTable,
    class_constants: MemberTable,
    /// A class → its traits, its base class and its interfaces, kept apart.
    /// All three contribute members; `in_hierarchy` walks them in PHP's order.
    parents: HashMap<String, Parents>,
    /// Classes with a base class or a trait outside the workspace: what they
    /// inherit is not known, so a method they do not declare may be there.
    foreign_parents: HashSet<String>,
    /// (class, property) → the class its type declaration names. A method's
    /// declared return class is kept here too, under the property name
    /// `()method`, which no property can have.
    property_types: HashMap<MemberKey, ClassRef>,
    /// (class, method) of methods declared to return `self` or `static`: what
    /// they return is the class they are called on.
    self_returning: HashSet<MemberKey>,
    /// The enums the workspace declares, as table keys: `E::A` is an `E`.
    enums: HashSet<String>,
    /// (document, lowercased short name) → the class it declares — the other
    /// half of the PSR-4 fallback.
    short_names: HashMap<(u32, String), String>,
    /// (document, line, column) of an `anonymous_class` node → its table key.
    /// The declaration pass is the only place that computes the name the
    /// visitor gives it (`<scope>\class@L:C`); the use pass looks it up here.
    anonymous: HashMap<(u32, u32, u32), String>,
    /// Names held over until every file has been indexed.
    pending: Vec<Pending>,
    /// PSR-4 prefix → directory relative to the project root, longest prefix
    /// first.
    psr4: Vec<(String, String)>,
    /// The namespace prefixes the project declares, Composer's vendor prefixes
    /// and PHP's built-in classes — for classifying what the tables do not hold.
    internal: HashSet<String>,
    vendors: HashSet<String>,
    stdlib: HashSet<String>,
}

impl Default for PhpResolver {
    fn default() -> Self {
        Self {
            root: None,
            status: ResolverStatus::Unavailable,
            docs: Vec::new(),
            doc_index: HashMap::new(),
            answers: Vec::new(),
            uses: Vec::new(),
            classes: HashMap::new(),
            functions: HashMap::new(),
            constants: HashMap::new(),
            methods: HashMap::new(),
            properties: HashMap::new(),
            class_constants: HashMap::new(),
            parents: HashMap::new(),
            foreign_parents: HashSet::new(),
            property_types: HashMap::new(),
            self_returning: HashSet::new(),
            enums: HashSet::new(),
            short_names: HashMap::new(),
            anonymous: HashMap::new(),
            pending: Vec::new(),
            psr4: Vec::new(),
            internal: HashSet::new(),
            vendors: HashSet::new(),
            stdlib: HashSet::new(),
        }
    }
}

impl PhpResolver {
    pub fn empty() -> Self {
        Self::default()
    }

    // -- resolution ---------------------------------------------------------

    /// The candidates for a written name, most specific first.
    ///
    /// PHP's own order: a leading `\` settles it, then the file's aliases, then
    /// the current namespace, then the global one. The global fallback is real
    /// for functions and constants only — a class name never falls back — but it
    /// is offered for classes too, because code written before namespaces
    /// existed reaches global classes from inside one, and PHP would have failed
    /// loudly if such a name meant anything else.
    fn candidates(&self, name: &str, doc: u32, namespace: &str, kind: NameKind) -> Candidates {
        if let Some(absolute) = name.strip_prefix('\\') {
            return Candidates { names: vec![absolute.to_string()], bound: true };
        }
        let (head, rest) = match name.split_once('\\') {
            Some((head, rest)) => (head, Some(rest)),
            None => (name, None),
        };

        // `namespace\Foo` names the current namespace explicitly.
        if head.eq_ignore_ascii_case("namespace") {
            let names = match rest {
                Some(rest) => vec![qualify(namespace, rest)],
                None => vec![namespace.to_string()],
            };
            return Candidates { names, bound: true };
        }

        // Only the FIRST segment can be an alias. An unqualified function name
        // goes through `use function` aliases only, an unqualified constant
        // through `use const` only: PHP keeps classes in a separate table, so
        // a `use Lib\Once;` says nothing about `once()`. The head of a
        // qualified name is always a class-or-namespace alias —
        // `use App\Models; … Models\User` — whatever the name is used as.
        let uses = &self.uses[doc as usize];
        let head = head.to_lowercase();
        let alias = match (kind, rest) {
            (NameKind::Function, None) => uses.functions.get(&head),
            (NameKind::Value, None) => uses.constants.get(&head),
            _ => uses.classes.get(&head),
        };
        if let Some(target) = alias {
            let names = match rest {
                Some(rest) => vec![format!("{target}\\{rest}")],
                None => vec![target.clone()],
            };
            return Candidates { names, bound: true };
        }

        let mut names = Vec::with_capacity(2);
        if !namespace.is_empty() {
            names.push(qualify(namespace, name));
        }
        names.push(name.to_string());
        Candidates { names, bound: false }
    }

    /// The name to report for a candidate list nothing declared.
    ///
    /// A class means the most specific candidate, because PHP does not fall back
    /// to global for one. A function or a constant means the last, which is the
    /// global name PHP would have reached.
    fn reportable(candidates: &Candidates, kind: NameKind) -> String {
        let chosen = if candidates.bound || matches!(kind, NameKind::Class) {
            candidates.names.first()
        } else {
            candidates.names.last()
        };
        chosen.cloned().unwrap_or_default()
    }

    fn external(&self, name: String) -> Answer {
        let origin = classify_import(&name, &self.internal, &self.vendors);
        Answer::External { name, origin }
    }

    /// The class a written name refers to.
    fn class_ref(&self, name: &str, doc: u32, namespace: &str) -> ClassRef {
        let candidates = self.candidates(name, doc, namespace, NameKind::Class);
        for candidate in &candidates.names {
            let key = candidate.to_lowercase();
            if self.classes.contains_key(&key) {
                return ClassRef::Declared(key);
            }
        }
        for candidate in &candidates.names {
            if let Some(key) = self.psr4_lookup(candidate) {
                return ClassRef::Declared(key);
            }
        }
        // A bare built-in is global wherever it is written: code inside a
        // namespace that means something else by `Exception` does not run.
        let bare = name.trim_start_matches('\\');
        if !bare.contains('\\') && self.stdlib.contains(bare) {
            return ClassRef::Foreign(bare.to_string());
        }
        ClassRef::Foreign(Self::reportable(&candidates, NameKind::Class))
    }

    /// What a use-site of a class answers with: the declaration, or the real name of
    /// a class the workspace does not declare (a vendor base class).
    fn class_answer(&self, class: &ClassRef) -> Answer {
        match class {
            ClassRef::Declared(key) => match self.classes.get(key) {
                Some(entry) => Answer::Internal(entry.site),
                None => self.external(key.clone()),
            },
            ClassRef::Foreign(fqn) => self.external(fqn.clone()),
        }
    }

    /// Like `class_ref`, but `self`, `static` and `parent` are the scope's classes
    /// and never a name to qualify. None when the scope has no such class.
    fn scoped_class_ref(&self, name: &str, doc: u32, scope: &UseScope) -> Option<ClassRef> {
        match relative_keyword(name) {
            Some(which) => relative_class(scope, which),
            None => Some(self.class_ref(name, doc, &scope.namespace)),
        }
    }

    /// The class declared by the file PSR-4 maps the name to.
    ///
    /// Composer maps a namespace prefix to a directory, so `App\Domain\Engine`
    /// under `"App\\": "src/"` must live in `src/Domain/Engine.php`. Reaching
    /// the class through its file rather than through its declared name still
    /// resolves one whose file spells the namespace differently — which happens
    /// in real projects, and which the autoloader forgives as long as the file
    /// is where it belongs.
    fn psr4_lookup(&self, fqn: &str) -> Option<String> {
        let fqn = fqn.trim_start_matches('\\');
        // The map is sorted longest prefix first, so the first match is the one
        // Composer itself would have used.
        for (prefix, dir) in &self.psr4 {
            let Some(tail) = fqn.strip_prefix(prefix.as_str()) else {
                continue;
            };
            let tail = tail.trim_start_matches('\\');
            if tail.is_empty() {
                continue;
            }
            let path = tail.replace('\\', "/");
            let relative = if dir.is_empty() {
                format!("{path}.php")
            } else {
                format!("{dir}/{path}.php")
            };
            let Some(doc) = self.doc_index.get(&relative) else {
                continue;
            };
            let short = tail.rsplit('\\').next().unwrap_or(tail).to_lowercase();
            if let Some(key) = self.short_names.get(&(*doc, short)) {
                return Some(key.clone());
            }
        }
        None
    }

    /// The declared spelling of a class, for naming a member of it.
    ///
    /// One lifetime for both arguments: the answer comes from the class table
    /// for a declared class and from the reference itself for a foreign one.
    fn class_fqn<'a>(&'a self, class: &'a ClassRef) -> &'a str {
        match class {
            ClassRef::Declared(key) => self
                .classes
                .get(key)
                .map_or(key.as_str(), |entry| entry.fqn.as_str()),
            ClassRef::Foreign(fqn) => fqn.as_str(),
        }
    }

    fn member_table(&self, slot: Slot) -> &MemberTable {
        match slot {
            Slot::Method | Slot::StaticMethod => &self.methods,
            Slot::Property => &self.properties,
            Slot::Constant => &self.class_constants,
        }
    }

    /// PHP compares method names case-insensitively; property and constant
    /// names it compares exactly.
    fn member_key(slot: Slot, member: &str) -> String {
        match slot {
            Slot::Method | Slot::StaticMethod => member.to_lowercase(),
            Slot::Property | Slot::Constant => member.to_string(),
        }
    }

    /// Walks a class in the order PHP finds a member: the class itself, its
    /// traits (a trait may `use` others), then its base class and the base's own
    /// traits and base. Interfaces are searched in a second pass, only when no
    /// class or trait in the chain has the member: an interface method is the
    /// target only when nothing implements it on the way.
    ///
    /// The visited set is not an optimization: PHP rejects a cycle, but a
    /// half-written file can still describe one and this must not hang.
    fn in_hierarchy<T>(&self, class: &str, probe: impl Fn(&str) -> Option<T>) -> Option<T> {
        let mut seen: HashSet<String> = HashSet::new();
        // Interfaces met in the first pass, in visit order: the roots of the second.
        let mut interfaces: Vec<String> = Vec::new();
        let mut stack = vec![class.to_string()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(found) = probe(&current) {
                return Some(found);
            }
            if let Some(parents) = self.parents.get(&current) {
                interfaces.extend(parents.interfaces.iter().cloned());
                // Pushed last, popped first: traits before the base class.
                stack.extend(parents.bases.iter().rev().cloned());
                stack.extend(parents.traits.iter().rev().cloned());
            }
        }
        let mut stack: Vec<String> = interfaces.into_iter().rev().collect();
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(found) = probe(&current) {
                return Some(found);
            }
            if let Some(parents) = self.parents.get(&current) {
                stack.extend(parents.traits.iter().rev().cloned());
                stack.extend(parents.bases.iter().rev().cloned());
                stack.extend(parents.interfaces.iter().rev().cloned());
            }
        }
        None
    }

    fn find_member(&self, class: &str, member: &str, slot: Slot) -> Option<Site> {
        let key = Self::member_key(slot, member);
        let table = self.member_table(slot);
        self.in_hierarchy(class, |owner| {
            table.get(&(owner.to_string(), key.clone())).copied()
        })
    }

    fn property_type(&self, class: &str, property: &str) -> Option<ClassRef> {
        self.in_hierarchy(class, |owner| {
            self.property_types
                .get(&(owner.to_string(), property.to_string()))
                .cloned()
        })
    }

    /// The answer for `receiver::member`, once the receiver's class is known.
    fn member_answer(&self, class: &ClassRef, member: &str, slot: Slot) -> Answer {
        if let ClassRef::Declared(key) = class
            && let Some(site) = self.find_member(key, member, slot)
        {
            return Answer::Internal(site);
        }
        // A method no class of the hierarchy declares is a call of `__call` (or,
        // written as `A::m()`, of `__callStatic`) when one of them has it.
        if let ClassRef::Declared(key) = class
            && matches!(slot, Slot::Method | Slot::StaticMethod)
            && self.in_hierarchy(key, |c| self.foreign_parents.contains(c).then_some(())).is_none()
        {
            let magic: &[&str] = if matches!(slot, Slot::StaticMethod) {
                &["__callstatic", "__call"]
            } else {
                &["__call"]
            };
            for name in magic {
                if let Some(site) = self.find_member(key, name, Slot::Method) {
                    return Answer::Internal(site);
                }
            }
        }
        // The class is known and the member is not: it comes from a dependency's
        // base class, or from `__call`. Naming it keeps the edge, pointed at an
        // EXTERNAL_SYMBOL for the member rather than at nothing.
        let name = format!("{}\\{member}", self.class_fqn(class));
        self.external(name)
    }

    fn declared_class(&self, fqn: &str) -> Option<Site> {
        self.classes.get(&fqn.to_lowercase()).map(|entry| entry.site)
    }

    fn declared_function(&self, fqn: &str) -> Option<Site> {
        self.functions.get(&fqn.to_lowercase()).copied()
    }

    fn declared_constant(&self, fqn: &str) -> Option<Site> {
        self.constants.get(fqn).copied()
    }

    /// The answer for the name a `use` clause imports. A use path is always fully
    /// qualified, so it never goes through `candidates()`: the file's own aliases
    /// would otherwise rewrite it.
    fn use_answer(&self, doc: u32, fqn: &str, kind: &str) -> Answer {
        match kind {
            "function" => match self.declared_function(fqn) {
                Some(site) => Answer::Internal(site),
                None => self.external(fqn.to_string()),
            },
            "const" => match self.declared_constant(fqn) {
                Some(site) => Answer::Internal(site),
                None => self.external(fqn.to_string()),
            },
            _ => match self.class_ref(&format!("\\{fqn}"), doc, "") {
                ClassRef::Declared(key) => match self.classes.get(&key) {
                    Some(entry) => Answer::Internal(entry.site),
                    None => self.external(fqn.to_string()),
                },
                ClassRef::Foreign(_) => self.external(fqn.to_string()),
            },
        }
    }

    /// The answer for a name written in an expression or a type.
    ///
    /// Never None: a name that can be read is a name that can be reported, and
    /// an EXTERNAL_SYMBOL keeps the edge the occurrence stands for.
    fn name_answer(&self, name: &str, doc: u32, namespace: &str, kind: NameKind) -> Answer {
        let candidates = self.candidates(name, doc, namespace, kind);
        for candidate in &candidates.names {
            // The order is the one the syntax implies: a call names only a
            // function (PHP never calls a class by `f()`), a type a class, a
            // bare read anything at all.
            let found = match kind {
                NameKind::Class => self.declared_class(candidate),
                NameKind::Function => self.declared_function(candidate),
                NameKind::Value => self
                    .declared_class(candidate)
                    .or_else(|| self.declared_constant(candidate))
                    .or_else(|| self.declared_function(candidate)),
            };
            if let Some(site) = found {
                return Answer::Internal(site);
            }
        }
        // A class the tables miss may still be reachable through PSR-4. A bare name
        // in value position can also meet a class this way; `::` qualifiers no longer
        // come here, they are class positions (`walk_class_qualifier`).
        if matches!(kind, NameKind::Class | NameKind::Value)
            && let ClassRef::Declared(key) = self.class_ref(name, doc, namespace)
            && let Some(entry) = self.classes.get(&key)
        {
            return Answer::Internal(entry.site);
        }
        self.external(Self::reportable(&candidates, kind))
    }

    // -- the first pass: declarations --------------------------------------

    fn intern_doc(&mut self, relative: &str) -> u32 {
        if let Some(index) = self.doc_index.get(relative) {
            return *index;
        }
        let index = self.docs.len() as u32;
        self.docs.push(relative.to_string());
        self.doc_index.insert(relative.to_string(), index);
        self.answers.push(Answers::new());
        self.uses.push(Uses::default());
        index
    }

    fn site(doc: u32, name_node: TsNode<'_>) -> Site {
        let span = ts::span(name_node);
        Site { doc, line: span.start_line, col: span.start_col }
    }

    fn walk_decl_children(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &mut DeclScope,
    ) {
        for child in ts::children(node) {
            self.walk_decls(doc, source, child, scope);
        }
    }

    /// The `use` statements of one file, the group form included.
    fn index_use_declaration(&mut self, doc: u32, source: &[u8], node: TsNode<'_>) {
        let declared = use_kind(node);
        let (prefix, clauses) = match node.child_by_field_name("body") {
            // `use App\{Cache, Logger as Log};` keeps its prefix as a
            // positional child while the clauses hold bare names.
            Some(group) => (
                ts::child_of_type(node, "namespace_name")
                    .map(|n| ts::text(n, source).into_owned())
                    .unwrap_or_default(),
                ts::children(group),
            ),
            None => (String::new(), ts::children(node)),
        };

        for clause in clauses {
            if clause.kind() != "namespace_use_clause" {
                continue;
            }
            let Some(path_node) = ts::child_of_types(clause, &["qualified_name", "name"]) else {
                continue;
            };
            let path = ts::text(path_node, source);
            let path = path.trim_matches('\\');
            let target = if prefix.is_empty() {
                path.to_string()
            } else {
                format!("{}\\{path}", prefix.trim_matches('\\'))
            };
            if target.is_empty() {
                continue;
            }
            let local = match clause.child_by_field_name("alias") {
                Some(alias) => ts::text(alias, source).into_owned(),
                None => target.rsplit('\\').next().unwrap_or(&target).to_string(),
            };
            // A clause inside a group carries its own kind:
            // `use App\{function slugify, const LIMIT, Cache};`.
            let kind = match use_kind(clause) {
                "class" => declared,
                own => own,
            };
            let uses = &mut self.uses[doc as usize];
            let table = match kind {
                "function" => &mut uses.functions,
                "const" => &mut uses.constants,
                _ => &mut uses.classes,
            };
            table.insert(local.to_lowercase(), target);
        }
    }

    fn walk_decls(&mut self, doc: u32, source: &[u8], node: TsNode<'_>, scope: &mut DeclScope) {
        match node.kind() {
            "namespace_definition" => {
                let name = namespace_of(node, source);
                match node.child_by_field_name("body") {
                    // The braces bound the namespace.
                    Some(body) => {
                        let mut inner = DeclScope {
                            namespace: name.clone(),
                            class: scope.class.clone(),
                            prefix: name,
                        };
                        self.walk_decl_children(doc, source, body, &mut inner);
                    }
                    // The statement form runs to the next one or to the end of
                    // the file, so it rewrites what its siblings see.
                    None => {
                        scope.prefix = name.clone();
                        scope.namespace = name;
                    }
                }
            }
            "namespace_use_declaration" => self.index_use_declaration(doc, source, node),
            "class_declaration" | "interface_declaration" | "trait_declaration"
            | "enum_declaration" => self.declare_class(doc, source, node, scope),
            "anonymous_class" => self.declare_anonymous_class(doc, source, node, scope),
            "method_declaration" => {
                if let Some(class) = scope.class.clone() {
                    let namespace = scope.namespace.clone();
                    self.declare_method(doc, source, node, &class, &namespace);
                }
                let mut inner = Self::callable_decl_scope(scope, source, node);
                self.walk_decl_children(doc, source, node, &mut inner);
            }
            // A function declared inside a class body is a `method_declaration`
            // in this grammar, so a `function_definition` always belongs to its
            // namespace — even nested inside another function's body, which is
            // where PHP puts it too.
            "function_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let qname =
                        qualify(&scope.namespace, &ts::text(name_node, source)).to_lowercase();
                    self.functions.entry(qname).or_insert(Self::site(doc, name_node));
                }
                let mut inner = Self::callable_decl_scope(scope, source, node);
                self.walk_decl_children(doc, source, node, &mut inner);
            }
            "property_declaration" => self.declare_properties(doc, source, node, scope),
            "const_declaration" => self.declare_constants(doc, source, node, scope),
            "enum_case" => {
                if let (Some(class), Some(name_node)) =
                    (scope.class.clone(), node.child_by_field_name("name"))
                {
                    let name = ts::text(name_node, source).into_owned();
                    self.class_constants
                        .entry((class.to_lowercase(), name))
                        .or_insert(Self::site(doc, name_node));
                }
            }
            // `use SomeTrait;` in a class body: a trait contributes members the
            // way a base class does. The conflict-resolution block is skipped —
            // it names traits AND their methods with no field to tell them
            // apart, and reading it wholesale turns every method named there
            // into a phantom base class.
            "use_declaration" => {
                if let Some(class) = scope.class.clone() {
                    let namespace = scope.namespace.clone();
                    for child in ts::children(node) {
                        if matches!(child.kind(), "name" | "qualified_name" | "relative_name") {
                            self.push_pending(doc, source, &class, PendingRole::Parent(ParentKind::Trait), child, &namespace);
                        }
                    }
                }
            }
            _ => self.walk_decl_children(doc, source, node, scope),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_pending(
        &mut self,
        doc: u32,
        source: &[u8],
        class: &str,
        role: PendingRole,
        reference: TsNode<'_>,
        namespace: &str,
    ) {
        self.pending.push(Pending {
            class: class.to_lowercase(),
            role,
            name: ts::text(reference, source).into_owned(),
            doc,
            namespace: namespace.to_string(),
        });
    }

    fn declare_class(&mut self, doc: u32, source: &[u8], node: TsNode<'_>, scope: &DeclScope) {
        // A nameless node is skipped here: an anonymous class is declared by
        // `declare_anonymous_class`, under the visitor's synthetic name.
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = ts::text(name_node, source).into_owned();
        let fqn = qualify(&scope.namespace, &name);
        let key = fqn.to_lowercase();
        let site = Self::site(doc, name_node);
        self.classes
            .entry(key.clone())
            .or_insert_with(|| ClassEntry { fqn: fqn.clone(), site });
        self.short_names
            .entry((doc, name.to_lowercase()))
            .or_insert_with(|| key.clone());
        if node.kind() == "enum_declaration" {
            self.enums.insert(key.clone());
        }

        self.declare_bases(doc, source, node, &fqn, &scope.namespace);

        if let Some(body) = node.child_by_field_name("body") {
            let mut inner = DeclScope {
                namespace: scope.namespace.clone(),
                prefix: qualify(&scope.prefix, &name),
                class: Some(fqn),
            };
            self.walk_decl_children(doc, source, body, &mut inner);
        }
    }

    /// `extends` and `implements` only: an enum's backing type is a positional
    /// `primitive_type` sharing the child list with them, and a scan of the
    /// whole list would report `string` as an interface.
    fn declare_bases(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        class_fqn: &str,
        namespace: &str,
    ) {
        for clause in ts::children(node) {
            if !matches!(clause.kind(), "base_clause" | "class_interface_clause") {
                continue;
            }
            // `interface I extends J` lists interfaces; a class's `extends` is a base.
            let kind = if clause.kind() == "base_clause" && node.kind() != "interface_declaration" {
                ParentKind::Base
            } else {
                ParentKind::Interface
            };
            for reference in class_refs(clause) {
                self.push_pending(
                    doc,
                    source,
                    class_fqn,
                    PendingRole::Parent(kind),
                    reference,
                    namespace,
                );
            }
        }
    }

    /// The scope a method or function body is walked in: the visitor pushes the
    /// callable's name, so an anonymous class inside it is named under it.
    fn callable_decl_scope(scope: &DeclScope, source: &[u8], node: TsNode<'_>) -> DeclScope {
        let prefix = match node.child_by_field_name("name") {
            Some(name_node) => qualify(&scope.prefix, &ts::text(name_node, source)),
            None => scope.prefix.clone(),
        };
        DeclScope { namespace: scope.namespace.clone(), class: scope.class.clone(), prefix }
    }

    /// `new class(…) extends Base { … }`. The visitor names the CLASS node
    /// `<scope>\class@L:C` from the node's own start; this pass computes the same
    /// name, keys the class by it and remembers the key by position. The name
    /// cannot be written in PHP (`@`), so it is kept out of `short_names`.
    fn declare_anonymous_class(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &mut DeclScope,
    ) {
        let span = ts::span(node);
        let name = format!("class@{}:{}", span.start_line, span.start_col);
        let fqn = qualify(&scope.prefix, &name);
        let key = fqn.to_lowercase();
        let site = Self::site(doc, ts::child_of_type(node, "class").unwrap_or(node));
        self.classes
            .entry(key.clone())
            .or_insert_with(|| ClassEntry { fqn: fqn.clone(), site });
        self.anonymous.insert((doc, span.start_line, span.start_col), key);
        self.declare_bases(doc, source, node, &fqn, &scope.namespace);

        // The constructor arguments belong to the code that wrote `new`.
        if let Some(arguments) = ts::child_of_type(node, "arguments") {
            self.walk_decl_children(doc, source, arguments, scope);
        }
        let mut inner = DeclScope {
            namespace: scope.namespace.clone(),
            class: Some(fqn.clone()),
            prefix: fqn,
        };
        for field in ["attributes", "body"] {
            if let Some(child) = node.child_by_field_name(field) {
                self.walk_decl_children(doc, source, child, &mut inner);
            }
        }
    }

    fn declare_method(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        class: &str,
        namespace: &str,
    ) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = ts::text(name_node, source).to_lowercase();
        self.methods
            .entry((class.to_lowercase(), name.clone()))
            .or_insert(Self::site(doc, name_node));

        // What the method is declared to return: `self`/`static` is the class it is
        // called on, one class name is that class, anything else (a union, a
        // built-in) names no single receiver and is left unresolved.
        if let Some(return_type) = node.child_by_field_name("return_type") {
            let text = ts::text(return_type, source).trim_start_matches('?').to_lowercase();
            if text == "self" || text == "static" {
                self.self_returning.insert((class.to_lowercase(), name.clone()));
            } else if let [only] = class_refs(return_type)[..] {
                self.push_pending(doc, source, class, PendingRole::Property(format!("(){name}")), only, namespace);
            }
        }

        // A promoted constructor parameter is a parameter and a property at
        // once, and the property is what the rest of the class refers to.
        let Some(parameters) = node.child_by_field_name("parameters") else {
            return;
        };
        for parameter in ts::children(parameters) {
            if parameter.kind() != "property_promotion_parameter" {
                continue;
            }
            let Some(ident) = parameter
                .child_by_field_name("name")
                .and_then(variable_identifier)
            else {
                continue;
            };
            let type_node = parameter.child_by_field_name("type");
            self.declare_property(doc, source, class, namespace, ident, type_node);
        }
    }

    fn declare_properties(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &DeclScope,
    ) {
        let Some(class) = scope.class.clone() else {
            return;
        };
        let type_node = node.child_by_field_name("type");
        for element in ts::children(node) {
            if element.kind() != "property_element" {
                continue;
            }
            let Some(ident) = element
                .child_by_field_name("name")
                .and_then(variable_identifier)
            else {
                continue;
            };
            self.declare_property(doc, source, &class, &scope.namespace, ident, type_node);
        }
    }

    fn declare_property(
        &mut self,
        doc: u32,
        source: &[u8],
        class: &str,
        namespace: &str,
        ident: TsNode<'_>,
        type_node: Option<TsNode<'_>>,
    ) {
        let name = ts::text(ident, source).into_owned();
        self.properties
            .entry((class.to_lowercase(), name.clone()))
            .or_insert(Self::site(doc, ident));

        // One class name types the property; a union type names no single
        // receiver, so it is left unresolved rather than guessed at.
        if let Some(type_node) = type_node {
            let references = class_refs(type_node);
            if let [only] = references[..] {
                self.push_pending(doc, source, class, PendingRole::Property(name.clone()), only, namespace);
            }
        }
    }

    fn declare_constants(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &DeclScope,
    ) {
        for element in ts::children(node) {
            if element.kind() != "const_element" {
                continue;
            }
            // No field names: `[name, =, expression]`, and the value may be a
            // name of its own, so the FIRST one is the constant.
            let Some(name_node) = ts::child_of_type(element, "name") else {
                continue;
            };
            let name = ts::text(name_node, source).into_owned();
            let site = Self::site(doc, name_node);
            match &scope.class {
                Some(class) => {
                    self.class_constants
                        .entry((class.to_lowercase(), name))
                        .or_insert(site);
                }
                None => {
                    self.constants
                        .entry(qualify(&scope.namespace, &name))
                        .or_insert(site);
                }
            }
        }
    }

    /// Resolves the names held over from the first pass.
    fn link(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for item in pending {
            // A property typed `self` or `static` holds the class it is declared in.
            // `parent` cannot be told here (the base link is being built in this very
            // loop), so that property stays untyped rather than guessed.
            let is_property = matches!(item.role, PendingRole::Property(_));
            let keyword = relative_keyword(&item.name).filter(|_| is_property);
            let class = match keyword {
                Some(Relative::Enclosing) => ClassRef::Declared(item.class.clone()),
                Some(Relative::Parent) => continue,
                None => self.class_ref(&item.name, item.doc, &item.namespace),
            };
            match item.role {
                PendingRole::Property(property) => {
                    self.property_types.insert((item.class, property), class);
                }
                // A parent outside the workspace contributes no members this
                // index could find, so the link is not worth keeping. An
                // undeclared base class or trait makes the class foreign; an
                // interface contributes no members, so it does not.
                PendingRole::Parent(kind) => {
                    if let ClassRef::Declared(parent) = class {
                        let parents = self.parents.entry(item.class).or_default();
                        match kind {
                            ParentKind::Base => parents.bases.push(parent),
                            ParentKind::Trait => parents.traits.push(parent),
                            ParentKind::Interface => parents.interfaces.push(parent),
                        }
                    } else if matches!(kind, ParentKind::Base | ParentKind::Trait) {
                        self.foreign_parents.insert(item.class);
                    }
                }
            }
        }
    }

    // -- the second pass: use sites ----------------------------------------

    fn record(&mut self, doc: u32, position: TsNode<'_>, answer: Answer) {
        let span = ts::span(position);
        self.answers[doc as usize].insert((span.start_line, span.start_col), answer);
    }

    /// An answer for a name-ish node, recorded where the visitor recorded its
    /// occurrence: at the node's last `name` token.
    fn record_name(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        namespace: &str,
        kind: NameKind,
    ) {
        let Some(position) = last_name(node) else {
            return;
        };
        let name = ts::text(node, source).into_owned();
        // Syntax, not symbols: the visitor records no query here either.
        if node.kind() == "name"
            && match kind {
                NameKind::Function => super::builtins::is_language_construct(&name),
                NameKind::Value => super::builtins::is_magic_constant(&name),
                NameKind::Class => false,
            }
        {
            return;
        }
        let answer = self.name_answer(&name, doc, namespace, kind);
        self.record(doc, position, answer);
    }

    fn record_member(
        &mut self,
        doc: u32,
        position: TsNode<'_>,
        class: &ClassRef,
        member: &str,
        slot: Slot,
    ) {
        let answer = self.member_answer(class, member, slot);
        self.record(doc, position, answer);
    }

    /// Every class name a type declaration mentions, so a union type binds to
    /// each member of it rather than to its first.
    fn record_type(&mut self, doc: u32, source: &[u8], type_node: TsNode<'_>, scope: &UseScope) {
        for reference in class_refs(type_node) {
            self.record_class_name(doc, source, reference, scope);
        }
    }

    /// A class named in a type, `instanceof` or similar position. `self`, `static`
    /// and `parent` answer with the scope's class; with no such class they answer
    /// nothing (counted unresolved) instead of a made-up `App\self`.
    fn record_class_name(&mut self, doc: u32, source: &[u8], node: TsNode<'_>, scope: &UseScope) {
        if node.kind() == "name"
            && let Some(which) = relative_keyword(&ts::text(node, source))
        {
            if let Some(class) = relative_class(scope, which) {
                let answer = self.class_answer(&class);
                self.record(doc, node, answer);
            }
            return;
        }
        self.record_name(doc, source, node, &scope.namespace, NameKind::Class);
    }

    /// The left side of `::` (`Foo::m()`, `Foo::$p`, `Foo::C`, `Foo::class`) is a
    /// class position, like a type, `new` or `instanceof`: a name there binds to a
    /// class, interface, trait or enum and never to a function or constant of the
    /// same name (`Str::` must not reach `str()`). Anything that is not a plain
    /// name (`self`, `static`, `parent`, `$x`, an expression) goes through
    /// `walk_uses`, which already handles it.
    fn walk_class_qualifier(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &mut UseScope,
    ) {
        if matches!(node.kind(), "name" | "qualified_name" | "relative_name") {
            self.record_class_name(doc, source, node, scope);
        } else {
            self.walk_uses(doc, source, node, scope);
        }
    }

    /// The class an expression evaluates to, as far as a symbol table can tell.
    fn infer(
        &self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &UseScope,
    ) -> Option<ClassRef> {
        match node.kind() {
            "variable_name" => {
                let name = ts::text(variable_identifier(node)?, source).into_owned();
                if name == "this" {
                    return scope.class.clone();
                }
                scope
                    .vars
                    .get(&name)
                    .cloned()
                    .or_else(|| scope.class_strings.get(&name).cloned().flatten())
            }
            // `self`, `static`, `parent`.
            "relative_scope" => match ts::text(node, source).to_lowercase().as_str() {
                "parent" => scope.parent.clone(),
                _ => scope.class.clone(),
            },
            "name" | "qualified_name" | "relative_name" => {
                self.scoped_class_ref(&ts::text(node, source), doc, scope)
            }
            "object_creation_expression" => {
                if let Some(anonymous) = ts::child_of_type(node, "anonymous_class") {
                    let span = ts::span(anonymous);
                    return self
                        .anonymous
                        .get(&(doc, span.start_line, span.start_col))
                        .cloned()
                        .map(ClassRef::Declared);
                }
                let class =
                    ts::child_of_types(node, &["name", "qualified_name", "relative_name"])?;
                self.scoped_class_ref(&ts::text(class, source), doc, scope)
            }
            "member_access_expression" | "nullsafe_member_access_expression" => {
                let object = node.child_by_field_name("object")?;
                let name_node = node.child_by_field_name("name")?;
                if name_node.kind() != "name" {
                    return None;
                }
                let ClassRef::Declared(key) = self.infer(doc, source, object, scope)? else {
                    return None;
                };
                self.property_type(&key, &ts::text(name_node, source))
            }
            "scoped_property_access_expression" => {
                let owner = node.child_by_field_name("scope")?;
                let ident = node
                    .child_by_field_name("name")
                    .and_then(variable_identifier)?;
                let ClassRef::Declared(key) = self.infer(doc, source, owner, scope)? else {
                    return None;
                };
                self.property_type(&key, &ts::text(ident, source))
            }
            "parenthesized_expression" => {
                let inner = ts::named_children(node).into_iter().next()?;
                self.infer(doc, source, inner, scope)
            }
            // `E::A`: a case of an enum is an instance of it.
            "class_constant_access_expression" => {
                let children: Vec<TsNode<'_>> = ts::children(node)
                    .into_iter()
                    .filter(|c| !matches!(c.kind(), "comment" | "text_interpolation"))
                    .collect();
                let (&qualifier, &member) = (children.first()?, children.last()?);
                if children.len() < 2
                    || member.kind() != "name"
                    || ts::text(member, source).eq_ignore_ascii_case("class")
                {
                    return None;
                }
                let ClassRef::Declared(key) = self.infer(doc, source, qualifier, scope)? else {
                    return None;
                };
                self.enums.contains(&key).then_some(ClassRef::Declared(key))
            }
            // `A::create()->run()`, `$a->self()->run()`: a method declared to return
            // a class, or `self`/`static`, tells what the call evaluates to. Only a
            // declared return type does: inferring one from a body would take the
            // type checker this resolver stands in for.
            "member_call_expression" | "nullsafe_member_call_expression" | "scoped_call_expression" => {
                let name_node = node.child_by_field_name("name")?;
                if name_node.kind() != "name" {
                    return None;
                }
                let owner = node
                    .child_by_field_name("object")
                    .or_else(|| node.child_by_field_name("scope"))?;
                let ClassRef::Declared(key) = self.infer(doc, source, owner, scope)? else {
                    return None;
                };
                let method = ts::text(name_node, source).to_lowercase();
                self.in_hierarchy(&key, |class| {
                    if self.self_returning.contains(&(class.to_string(), method.clone())) {
                        return Some(ClassRef::Declared(key.clone()));
                    }
                    self.property_types.get(&(class.to_string(), format!("(){method}"))).cloned()
                })
            }
            // A function's or a container's return type is out of reach: it
            // would take the type checker this resolver stands in for.
            _ => None,
        }
    }

    /// The state a callable's body is walked in.
    ///
    /// A method sees `$this`, and so does a closure written inside one; a named
    /// function declared in the same place does not. That is why the class
    /// travels with the scope for the first two and not for the third, and why a
    /// closure inherits the locals its `use` clause may capture.
    fn callable_scope(scope: &UseScope, inherits: bool) -> UseScope {
        UseScope {
            namespace: scope.namespace.clone(),
            class: if inherits { scope.class.clone() } else { None },
            parent: if inherits { scope.parent.clone() } else { None },
            vars: if inherits { scope.vars.clone() } else { HashMap::new() },
            strings: if inherits { scope.strings.clone() } else { HashMap::new() },
            class_strings: if inherits { scope.class_strings.clone() } else { HashMap::new() },
        }
    }

    /// Binds the parameters of a callable, and records their types.
    fn bind_parameters(
        &mut self,
        doc: u32,
        source: &[u8],
        parameters: TsNode<'_>,
        scope: &mut UseScope,
    ) {
        for parameter in ts::children(parameters) {
            if !matches!(
                parameter.kind(),
                "simple_parameter" | "variadic_parameter" | "property_promotion_parameter"
            ) {
                continue;
            }
            // What a parameter holds is whatever the caller passes.
            if let Some(ident) = parameter.child_by_field_name("name").and_then(variable_identifier) {
                let name = ts::text(ident, source).into_owned();
                scope.strings.insert(name.clone(), None);
                scope.class_strings.insert(name, None);
            }
            if let Some(type_node) = parameter.child_by_field_name("type") {
                self.record_type(doc, source, type_node, scope);
                let references = class_refs(type_node);
                // One class names a receiver; a union names none.
                if let [only] = references[..]
                    && let Some(ident) = parameter
                        .child_by_field_name("name")
                        .and_then(variable_identifier)
                    && let Some(class) = self.scoped_class_ref(&ts::text(only, source), doc, scope)
                {
                    scope
                        .vars
                        .insert(ts::text(ident, source).into_owned(), class);
                }
            }
            if let Some(attributes) = parameter.child_by_field_name("attributes") {
                self.walk_use_children(doc, source, attributes, scope);
            }
            if let Some(default) = parameter.child_by_field_name("default_value") {
                self.walk_uses(doc, source, default, scope);
            }
        }
    }

    fn walk_callable(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &UseScope,
        inherits: bool,
    ) {
        let mut inner = Self::callable_scope(scope, inherits);
        if let Some(attributes) = node.child_by_field_name("attributes") {
            self.walk_use_children(doc, source, attributes, &mut inner);
        }
        if let Some(parameters) = node.child_by_field_name("parameters") {
            self.bind_parameters(doc, source, parameters, &mut inner);
        }
        if let Some(return_type) = node.child_by_field_name("return_type") {
            self.record_type(doc, source, return_type, &inner);
        }
        // Absent on an abstract method and on an interface's members.
        if let Some(body) = node.child_by_field_name("body") {
            self.walk_uses(doc, source, body, &mut inner);
        }
    }

    fn walk_use_children(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &mut UseScope,
    ) {
        for child in ts::children(node) {
            self.walk_uses(doc, source, child, scope);
        }
    }

    /// `Foo::class`: the class the expression names.
    fn class_constant_name(&self, doc: u32, source: &[u8], node: TsNode<'_>, scope: &UseScope) -> Option<ClassRef> {
        if node.kind() != "class_constant_access_expression" {
            return None;
        }
        let children: Vec<TsNode<'_>> = ts::children(node)
            .into_iter()
            .filter(|c| !matches!(c.kind(), "comment" | "text_interpolation"))
            .collect();
        let (&qualifier, &member) = (children.first()?, children.last()?);
        if children.len() < 2 || !ts::text(member, source).eq_ignore_ascii_case("class") {
            return None;
        }
        self.infer(doc, source, qualifier, scope)
    }

    /// What a callable written out names: the function of `'f'`, or the method of
    /// `[$object, 'm']` and `'Class::m'`.
    fn callable_answer(
        &self,
        doc: u32,
        source: &[u8],
        literal: &CallableLiteral<'_>,
        scope: &UseScope,
    ) -> Option<Answer> {
        Some(match &literal.class {
            None => self.name_answer(&format!("\\{}", literal.text), doc, &scope.namespace, NameKind::Function),
            Some(class) => {
                let receiver = match class {
                    CallableClass::Named(name) => self.scoped_class_ref(name, doc, scope),
                    CallableClass::Expression(class) if class.kind() == "class_constant_access_expression" => {
                        self.class_constant_name(doc, source, *class, scope)
                    }
                    CallableClass::Expression(class) => self.infer(doc, source, *class, scope),
                };
                self.member_answer(&receiver?, &literal.text, Slot::Method)
            }
        })
    }

    fn walk_uses(&mut self, doc: u32, source: &[u8], node: TsNode<'_>, scope: &mut UseScope) {
        // A callable written out - `'f'`, `'A::m'`, `[$o, 'm']` - is answered where
        // it stands, when it names something the project declares: any other string
        // is just a string, and whether the value is ever called is for the flow
        // pass to say.
        if matches!(node.kind(), "string" | "encapsed_string" | "array_creation_expression")
            && let Some(CallableValue::Literal(literal)) = callable_value(node, source)
            && let Some(answer) = self.callable_answer(doc, source, &literal, scope)
            && matches!(answer, Answer::Internal(_))
        {
            self.record(doc, literal.position, answer);
        }
        match node.kind() {
            // The two `extras` the parser may put between any two children, and
            // the HTML a template interleaves with its PHP.
            "comment" | "text_interpolation" | "text" | "php_tag" | "php_end_tag" => {}

            "namespace_definition" => {
                let name = namespace_of(node, source);
                match node.child_by_field_name("body") {
                    Some(body) => {
                        let mut inner = UseScope { namespace: name, ..UseScope::default() };
                        self.walk_use_children(doc, source, body, &mut inner);
                    }
                    None => scope.namespace = name,
                }
            }
            // Each clause is answered at the last name of the original path, which
            // is where the visitor records its `import` occurrence.
            "namespace_use_declaration" => {
                for clause in use_clauses(node, source) {
                    let answer = self.use_answer(doc, &clause.target, clause.kind);
                    self.record(doc, clause.last, answer);
                }
            }

            "class_declaration" | "interface_declaration" | "trait_declaration"
            | "enum_declaration" => self.walk_class(doc, source, node, scope),
            "anonymous_class" => self.walk_anonymous_class(doc, source, node, scope),

            "method_declaration" => self.walk_callable(doc, source, node, scope, true),
            "function_definition" => self.walk_callable(doc, source, node, scope, false),
            "anonymous_function" | "arrow_function" | "property_hook" => {
                self.walk_callable(doc, source, node, scope, true)
            }

            "property_declaration" | "const_declaration" | "enum_case" => {
                self.walk_member_declaration(doc, source, node, scope)
            }
            // A trait use in a class body, a class reference the way `extends`
            // is; the conflict-resolution block is left alone for the reason
            // the declaration pass gives.
            "use_declaration" => {
                let namespace = scope.namespace.clone();
                for child in ts::children(node) {
                    if matches!(child.kind(), "name" | "qualified_name" | "relative_name") {
                        self.record_name(doc, source, child, &namespace, NameKind::Class);
                    }
                }
            }

            "function_call_expression" => {
                let namespace = scope.namespace.clone();
                if let Some(function) = node.child_by_field_name("function") {
                    match function.kind() {
                        "name" | "qualified_name" | "relative_name" => self.record_name(
                            doc,
                            source,
                            function,
                            &namespace,
                            NameKind::Function,
                        ),
                        // `$object()` calls the `__invoke` of the object's class.
                        "variable_name" => {
                            if let Some(receiver) = self.infer(doc, source, function, scope)
                                && let Some(ident) = variable_identifier(function)
                            {
                                self.record_member(doc, ident, &receiver, "__invoke", Slot::Method);
                            }
                            self.walk_uses(doc, source, function, scope);
                        }
                        // Calling the result of an expression: `$factory()`.
                        _ => self.walk_uses(doc, source, function, scope),
                    }
                }
                if let Some(arguments) = node.child_by_field_name("arguments") {
                    self.walk_uses(doc, source, arguments, scope);
                }
                // `array_map('f', …)`, `usort($x, [$this, 'cmp'])`: the callable is called.
                for literal in callable_literals(node, source) {
                    if let Some(answer) = self.callable_answer(doc, source, &literal, scope) {
                        self.record(doc, literal.position, answer);
                    }
                }
            }
            "object_creation_expression" => {
                let namespace = scope.namespace.clone();
                for child in ts::children(node) {
                    let keyword = child.kind() == "name"
                        && matches!(ts::text(child, source).to_ascii_lowercase().as_str(), "static" | "self" | "parent");
                    match child.kind() {
                        "name" | "qualified_name" | "relative_name" if !keyword => {
                            self.record_name(doc, source, child, &namespace, NameKind::Class);
                        }
                        // `new static()`: the class the keyword stands for.
                        "relative_scope" | "name" => {
                            if let Some(which) = relative_keyword(&ts::text(child, source))
                                && let Some(class) = relative_class(scope, which)
                            {
                                let answer = self.class_answer(&class);
                                self.record(doc, child, answer);
                            }
                        }
                        // `new $class()`: the class the variable is known to name.
                        "variable_name" => {
                            if let Some(ident) = variable_identifier(child)
                                && let Some(Some(ClassRef::Declared(key))) =
                                    scope.class_strings.get(ts::text(ident, source).as_ref())
                            {
                                let class = ClassRef::Declared(key.clone());
                                let answer = self.member_answer(&class, "__construct", Slot::Method);
                                let answer = match answer {
                                    Answer::Internal(_) => answer,
                                    _ => match self.declared_class(&key.clone()) {
                                        Some(site) => Answer::Internal(site),
                                        None => answer,
                                    },
                                };
                                self.record(doc, ident, answer);
                            }
                        }
                        _ => self.walk_uses(doc, source, child, scope),
                    }
                }
            }
            "member_call_expression" | "nullsafe_member_call_expression" => {
                self.walk_member(doc, source, node, scope, Slot::Method);
                if let Some(arguments) = node.child_by_field_name("arguments") {
                    self.walk_uses(doc, source, arguments, scope);
                }
            }
            "member_access_expression" | "nullsafe_member_access_expression" => {
                self.walk_member(doc, source, node, scope, Slot::Property)
            }
            "scoped_call_expression" => {
                self.walk_scoped(doc, source, node, scope, Slot::StaticMethod);
                if let Some(arguments) = node.child_by_field_name("arguments") {
                    self.walk_uses(doc, source, arguments, scope);
                }
            }
            "scoped_property_access_expression" => {
                self.walk_scoped(doc, source, node, scope, Slot::Property)
            }
            "class_constant_access_expression" => {
                self.walk_class_constant(doc, source, node, scope)
            }
            "attribute" => {
                let namespace = scope.namespace.clone();
                if let Some(name_node) =
                    ts::child_of_types(node, &["name", "qualified_name", "relative_name"])
                {
                    self.record_name(doc, source, name_node, &namespace, NameKind::Class);
                }
                // An attribute's argument list is under `parameters:`.
                if let Some(arguments) = node.child_by_field_name("parameters") {
                    self.walk_uses(doc, source, arguments, scope);
                }
            }
            // `$x instanceof Foo`: `instanceof` is the `operator:` field of an
            // ordinary binary expression, and its right operand is a type test
            // rather than a constant read.
            "binary_expression" => {
                let is_instanceof = node
                    .child_by_field_name("operator")
                    .is_some_and(|op| op.kind().eq_ignore_ascii_case("instanceof"));
                let operand = node
                    .child_by_field_name("right")
                    .filter(|n| matches!(n.kind(), "name" | "qualified_name" | "relative_name"));
                if is_instanceof
                    && let Some(operand) = operand
                {
                    self.record_class_name(doc, source, operand, scope);
                    if let Some(left) = node.child_by_field_name("left") {
                        self.walk_uses(doc, source, left, scope);
                    }
                    return;
                }
                self.walk_use_children(doc, source, node, scope);
            }
            "catch_clause" => self.walk_catch(doc, source, node, scope),
            "assignment_expression" | "reference_assignment_expression" => {
                // `$x = new Foo();` is the inference this resolver leans on
                // most, and the binding has to be made before the statements
                // that call through `$x` are walked.
                if let Some(left) = node.child_by_field_name("left")
                    && left.kind() == "variable_name"
                    && let Some(ident) = variable_identifier(left)
                    && let Some(right) = node.child_by_field_name("right")
                    && let Some(class) = self.infer(doc, source, right, scope)
                {
                    scope
                        .vars
                        .insert(ts::text(ident, source).into_owned(), class);
                }
                // `$m = 'go'`, `$c = Foo::class`: a name held in a local, for as long as
                // nothing else is assigned to it.
                if let Some(left) = node.child_by_field_name("left")
                    && left.kind() == "variable_name"
                    && let Some(ident) = variable_identifier(left)
                {
                    let name = ts::text(ident, source).into_owned();
                    let right = node.child_by_field_name("right");
                    let string = right
                        .filter(|r| matches!(r.kind(), "string" | "encapsed_string"))
                        .and_then(|r| string_literal(r, source));
                    let class = right.and_then(|r| self.class_constant_name(doc, source, r, scope));
                    let merged = |old: Option<&Option<String>>, new: Option<String>| match (old, new) {
                        (Some(Some(old)), Some(new)) if *old == new => Some(new),
                        (None, new) => new,
                        _ => None,
                    };
                    let value = merged(scope.strings.get(&name), string);
                    scope.strings.insert(name.clone(), value);
                    let known = match (scope.class_strings.get(&name), class) {
                        (Some(Some(old)), Some(new)) if *old == new => Some(new),
                        (None, new) => new,
                        _ => None,
                    };
                    scope.class_strings.insert(name, known);
                }
                self.walk_use_children(doc, source, node, scope);
            }
            // A named argument's label (`send(to: $user)`) is a `name` node
            // sitting where an expression would be, and the visitor skips it
            // for the same reason: nothing is called `to`.
            "argument" => {
                let label = node.child_by_field_name("name");
                for child in ts::children(node) {
                    if Some(child) == label {
                        continue;
                    }
                    self.walk_uses(doc, source, child, scope);
                }
            }
            "name" | "qualified_name" | "relative_name" => {
                let namespace = scope.namespace.clone();
                self.record_name(doc, source, node, &namespace, NameKind::Value);
            }
            // `self`, `static`, `parent` before `::`: the class the keyword stands for.
            "relative_scope" => {
                if let Some(which) = relative_keyword(&ts::text(node, source))
                    && let Some(class) = relative_class(scope, which)
                {
                    let answer = self.class_answer(&class);
                    self.record(doc, node, answer);
                }
            }
            // A local variable, `$this` included: nothing outside the function
            // can name it, so there is nothing to resolve.
            "variable_name" => {}

            _ => self.walk_use_children(doc, source, node, scope),
        }
    }

    /// A property, a constant or an enum case: its type, its attributes and its
    /// initialiser.
    fn walk_member_declaration(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &mut UseScope,
    ) {
        if let Some(type_node) = node.child_by_field_name("type") {
            self.record_type(doc, source, type_node, scope);
        }
        if let Some(attributes) = node.child_by_field_name("attributes") {
            self.walk_use_children(doc, source, attributes, scope);
        }
        for element in ts::children(node) {
            match element.kind() {
                "property_element" => {
                    if let Some(value) = element.child_by_field_name("default_value") {
                        self.walk_uses(doc, source, value, scope);
                    }
                }
                // A const element has no field names: `[name, =, expression]`.
                // Only what follows the name is a use-site — scanning from the
                // start would record the constant as a reference to itself.
                "const_element" => {
                    let mut past_name = false;
                    for child in ts::children(element) {
                        if !past_name {
                            past_name = child.kind() == "name";
                            continue;
                        }
                        self.walk_uses(doc, source, child, scope);
                    }
                }
                _ => {}
            }
        }
        // An enum case's value.
        if let Some(value) = node.child_by_field_name("value") {
            self.walk_uses(doc, source, value, scope);
        }
        if let Some(hooks) = ts::child_of_type(node, "property_hook_list") {
            self.walk_use_children(doc, source, hooks, scope);
        }
    }

    fn walk_class(&mut self, doc: u32, source: &[u8], node: TsNode<'_>, scope: &UseScope) {
        let namespace = scope.namespace.clone();
        let class = node.child_by_field_name("name").map(|name_node| {
            let fqn = qualify(&namespace, &ts::text(name_node, source));
            let key = fqn.to_lowercase();
            if self.classes.contains_key(&key) {
                ClassRef::Declared(key)
            } else {
                ClassRef::Foreign(fqn)
            }
        });

        let parent = self.walk_bases(doc, source, node, &namespace);

        let mut inner = UseScope { namespace, class, parent, ..UseScope::default() };
        if let Some(attributes) = node.child_by_field_name("attributes") {
            self.walk_use_children(doc, source, attributes, &mut inner);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk_use_children(doc, source, body, &mut inner);
        }
    }

    /// Records every name in the `extends`/`implements` clauses and returns the
    /// first `extends` — the only thing `parent::` means; an interface is not,
    /// and neither is a trait.
    fn walk_bases(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        namespace: &str,
    ) -> Option<ClassRef> {
        let mut parent = None;
        for clause in ts::children(node) {
            if !matches!(clause.kind(), "base_clause" | "class_interface_clause") {
                continue;
            }
            for reference in class_refs(clause) {
                if clause.kind() == "base_clause" && parent.is_none() {
                    parent = Some(self.class_ref(&ts::text(reference, source), doc, namespace));
                }
                self.record_name(doc, source, reference, namespace, NameKind::Class);
            }
        }
        parent
    }

    /// `new class(…) extends Base { … }`: the arguments are the creator's, the
    /// body is a scope of its own whose `$this`, `self` and `static` are the
    /// anonymous class and whose `parent` is its base. It captures nothing, so
    /// it starts with no local variables.
    fn walk_anonymous_class(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &mut UseScope,
    ) {
        let span = ts::span(node);
        let Some(key) = self.anonymous.get(&(doc, span.start_line, span.start_col)).cloned() else {
            self.walk_use_children(doc, source, node, scope);
            return;
        };
        let namespace = scope.namespace.clone();
        if let Some(arguments) = ts::child_of_type(node, "arguments") {
            self.walk_uses(doc, source, arguments, scope);
        }
        let parent = self.walk_bases(doc, source, node, &namespace);
        let mut inner = UseScope {
            namespace,
            class: Some(ClassRef::Declared(key)),
            parent,
            ..UseScope::default()
        };
        if let Some(attributes) = node.child_by_field_name("attributes") {
            self.walk_use_children(doc, source, attributes, &mut inner);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk_use_children(doc, source, body, &mut inner);
        }
    }

    /// The member half of `->` and `?->`, plus its receiver.
    fn walk_member(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &mut UseScope,
        slot: Slot,
    ) {
        if let Some(name_node) = node.child_by_field_name("name") {
            // `$this->$m()`: the method the variable is known to name.
            if name_node.kind() == "variable_name"
                && matches!(slot, Slot::Method)
                && let Some(ident) = variable_identifier(name_node)
                && let Some(Some(method)) = scope.strings.get(ts::text(ident, source).as_ref()).cloned()
                && let Some(receiver) = node
                    .child_by_field_name("object")
                    .and_then(|object| self.infer(doc, source, object, scope))
            {
                self.record_member(doc, ident, &receiver, &method, slot);
            }
            if name_node.kind() == "name" {
                let receiver = node
                    .child_by_field_name("object")
                    .and_then(|object| self.infer(doc, source, object, scope));
                // An unknown receiver means an unresolved occurrence, not a
                // guess: `$row->name` where `$row` came out of a database.
                if let Some(receiver) = receiver {
                    let member = ts::text(name_node, source).into_owned();
                    self.record_member(doc, name_node, &receiver, &member, slot);
                }
            } else {
                // A computed member name: `$obj->$prop`.
                self.walk_uses(doc, source, name_node, scope);
            }
        }
        if let Some(object) = node.child_by_field_name("object") {
            self.walk_uses(doc, source, object, scope);
        }
    }

    /// The member half of `::`, plus the class it is reached through.
    fn walk_scoped(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &mut UseScope,
        slot: Slot,
    ) {
        let owner = node.child_by_field_name("scope");
        let receiver = owner.and_then(|n| self.infer(doc, source, n, scope));

        if let Some(name_node) = node.child_by_field_name("name")
            && let Some(receiver) = &receiver
        {
            // `Foo::$bar` spells its member with a variable, where `$obj->$prop`
            // uses that same node kind for a computed name; only the scoped form
            // may unwrap it.
            let position = match name_node.kind() {
                "name" => Some(name_node),
                "variable_name" => variable_identifier(name_node),
                _ => None,
            };
            if let Some(position) = position {
                let member = ts::text(position, source).into_owned();
                self.record_member(doc, position, receiver, &member, slot);
            }
        }
        if let Some(owner) = owner {
            self.walk_class_qualifier(doc, source, owner, scope);
        }
    }

    /// `Foo::BAR`, `self::BAR`, `Foo::class`.
    fn walk_class_constant(
        &mut self,
        doc: u32,
        source: &[u8],
        node: TsNode<'_>,
        scope: &mut UseScope,
    ) {
        // `comment` is an `extra` and may sit between the two halves of
        // `Foo::BAR`, so positions are read off the significant children.
        let children: Vec<TsNode<'_>> = ts::children(node)
            .into_iter()
            .filter(|c| !matches!(c.kind(), "comment" | "text_interpolation"))
            .collect();
        let (Some(&qualifier), Some(&member)) = (children.first(), children.last()) else {
            return;
        };

        if children.len() > 1
            && member.kind() == "name"
            // `Foo::class` names the class, which the qualifier already
            // records; there is no constant of that name to bind.
            && !ts::text(member, source).eq_ignore_ascii_case("class")
            && let Some(receiver) = self.infer(doc, source, qualifier, scope)
        {
            let name = ts::text(member, source).into_owned();
            self.record_member(doc, member, &receiver, &name, Slot::Constant);
        }
        self.walk_class_qualifier(doc, source, qualifier, scope);
    }

    /// `catch (FooError | BarError $e) { … }` — a type position, and one that
    /// types the variable it binds.
    fn walk_catch(&mut self, doc: u32, source: &[u8], node: TsNode<'_>, scope: &mut UseScope) {
        if let Some(type_node) = node.child_by_field_name("type") {
            self.record_type(doc, source, type_node, scope);
            let references = class_refs(type_node);
            // A union of exception types names no single class.
            if let [only] = references[..]
                && let Some(ident) = node
                    .child_by_field_name("name")
                    .and_then(variable_identifier)
                && let Some(class) = self.scoped_class_ref(&ts::text(only, source), doc, scope)
            {
                scope
                    .vars
                    .insert(ts::text(ident, source).into_owned(), class);
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk_uses(doc, source, body, scope);
        }
    }

    // -- preparation and queries -------------------------------------------

    /// An absolute path → a path relative to the project root.
    fn relative(&self, file: &Path) -> String {
        let Some(root) = &self.root else {
            return file.to_string_lossy().into_owned();
        };
        let resolved = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
        resolved
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| file.to_string_lossy().into_owned())
    }

    /// The PSR-4 maps and the dependency names of every project root.
    ///
    /// Directories are made relative to the project root, so a PSR-4 path can
    /// be compared with a document path even when the composer package sits in
    /// a subdirectory of a monorepo.
    ///
    /// Installed `vendor/` packages are not roots (see `php_roots`), so their
    /// PSR-4 prefixes never become internal.
    fn load_manifests(&mut self, project_root: &Path) {
        for root in php_roots(project_root) {
            let base = root
                .strip_prefix(project_root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.internal.extend(internal_prefixes(&root));
            self.vendors.extend(dependencies(&root));
            for (prefix, dir) in psr4_roots(&root) {
                let dir = if base.is_empty() {
                    dir
                } else if dir.is_empty() {
                    base.clone()
                } else {
                    format!("{base}/{dir}")
                };
                self.psr4.push((prefix, dir));
            }
        }
        // The invariant `psr4_roots` establishes has to survive the merge:
        // `App\Domain\` must be tried before `App\`.
        self.psr4
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        self.psr4.dedup_by(|a, b| a.0 == b.0);
    }

    fn resolve_one(&self, file: &Path, line: u32, col: u32) -> Option<ResolvedRef> {
        let root = self.root.as_ref()?;
        let doc = *self.doc_index.get(&self.relative(file))?;
        match self.answers[doc as usize].get(&(line, col))? {
            Answer::Internal(site) => Some(ResolvedRef {
                full_name: String::new(),
                file_path: Some(
                    root.join(&self.docs[site.doc as usize])
                        .to_string_lossy()
                        .into_owned(),
                ),
                line: site.line,
                col: site.col,
                kind: String::new(),
                origin: "internal".to_string(),
            }),
            Answer::External { name, origin } => Some(ResolvedRef {
                full_name: name.clone(),
                file_path: None,
                line: 0,
                col: 0,
                kind: String::new(),
                origin: (*origin).to_string(),
            }),
        }
    }

    /// Indexes the workspace: declarations first, use-sites second.
    ///
    /// The order is load-bearing and the two passes cannot be merged. A use
    /// site's receiver is inferred from property types and inheritance links
    /// that routinely live in another file, so the whole symbol table has to
    /// exist before the first occurrence is looked at. Each file is therefore
    /// parsed twice, its tree dropped in between: holding every tree at once
    /// costs an order of magnitude more than holding the sources.
    pub fn prepare_rust(&mut self, project_root: &Path, files: &[PathBuf]) -> PyResult<()> {
        let root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        *self = Self::default();
        self.root = Some(root.clone());
        self.stdlib = stdlib_names();
        self.load_manifests(&root);

        let mut sources: Vec<(u32, Vec<u8>)> = Vec::new();
        for file in files {
            let Ok(source) = std::fs::read(file) else {
                continue;
            };
            let tree = super::parse_tree(&source)?;
            // A `.php` file holding nothing but HTML declares nothing and
            // refers to nothing.
            if !has_php_statements(tree.root_node()) {
                continue;
            }
            let relative = self.relative(file);
            let doc = self.intern_doc(&relative);
            let mut scope = DeclScope { namespace: String::new(), class: None, prefix: String::new() };
            self.walk_decl_children(doc, &source, tree.root_node(), &mut scope);
            sources.push((doc, source));
        }
        self.link();

        for (doc, source) in &sources {
            let tree = super::parse_tree(source)?;
            let mut scope = UseScope::default();
            self.walk_use_children(*doc, source, tree.root_node(), &mut scope);
        }

        // Whatever it resolved, a symbol table produces a weaker graph than a
        // type checker would have, and the status has to say so.
        self.status = if self.docs.is_empty() {
            ResolverStatus::Unavailable
        } else {
            ResolverStatus::Degraded
        };
        Ok(())
    }

    pub fn resolve_rust(&self, path: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(Path::new(path), line, col)
    }

    pub fn status_rust(&self) -> ResolverStatus {
        self.status
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl PhpResolver {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    /// Indexes the project's sources.
    ///
    /// Args:
    ///     project_root: the root every document path is relative to.
    ///     files: the sources to index; when omitted, every `.php` file under
    ///         the root except the vendored tree. `vendor/` is left out
    ///         deliberately — it is larger than the project itself by an order
    ///         of magnitude, and a call into it belongs in an EXTERNAL_SYMBOL.
    #[pyo3(signature = (project_root, files = None))]
    fn prepare(&mut self, project_root: PathBuf, files: Option<Vec<PathBuf>>) -> PyResult<()> {
        let files = files.unwrap_or_else(|| super::adapter::collect_php_files(&project_root));
        self.prepare_rust(&project_root, &files)
    }

    fn resolve_all(&self, queries: Vec<(PathBuf, u32, u32)>) -> Vec<Option<ResolvedRef>> {
        queries
            .into_iter()
            .map(|(path, line, col)| self.resolve_one(&path, line, col))
            .collect()
    }

    fn definition_at(&self, file: PathBuf, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(&file, line, col)
    }

    fn status(&self) -> ResolverStatus {
        self.status
    }

    fn __repr__(&self) -> String {
        format!(
            "PhpResolver(root={:?}, documents={}, classes={}, functions={}, status={})",
            self.root.as_ref().map(|r| r.to_string_lossy()),
            self.docs.len(),
            self.classes.len(),
            self.functions.len(),
            self.status.as_str()
        )
    }
}

/// The only backend that needs the file list: there is no manifest that names
/// every source, so the symbol table is built from exactly the files the walk
/// collected.
impl crate::resolver_slot::NativeResolver for PhpResolver {
    fn prepare(&mut self, project_root: &Path, files: &[PathBuf]) -> PyResult<()> {
        self.prepare_rust(project_root, files)
    }

    fn resolve(&self, path: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_rust(path, line, col)
    }

    fn status(&self) -> ResolverStatus {
        self.status_rust()
    }
}
