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
//! * `$this->m()`, `self::m()`, `static::m()` and `parent::m()`;
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

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};
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
#[derive(Clone)]
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
    /// The property this types, or None when the name is a base class, an
    /// interface or a trait.
    property: Option<String>,
    name: String,
    doc: u32,
    namespace: String,
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
    /// A class → its base class, its interfaces and its traits. All three
    /// contribute members, so all three are walked when a member is not
    /// declared on the class itself.
    parents: HashMap<String, Vec<String>>,
    /// (class, property) → the class its type declaration names.
    property_types: HashMap<MemberKey, ClassRef>,
    /// (document, lowercased short name) → the class it declares — the other
    /// half of the PSR-4 fallback.
    short_names: HashMap<(u32, String), String>,
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
            property_types: HashMap::new(),
            short_names: HashMap::new(),
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

        // Only the FIRST segment can be an alias. A `use function` alias names
        // a function and nothing else, but the head of a qualified name is
        // always a class-or-namespace alias — `use App\Models; … Models\User`.
        let uses = &self.uses[doc as usize];
        let head = head.to_lowercase();
        let alias = match kind {
            NameKind::Function if rest.is_none() => uses.functions.get(&head),
            NameKind::Value if rest.is_none() => uses.constants.get(&head),
            _ => None,
        }
        .or_else(|| uses.classes.get(&head));
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
            Slot::Method => &self.methods,
            Slot::Property => &self.properties,
            Slot::Constant => &self.class_constants,
        }
    }

    /// PHP compares method names case-insensitively; property and constant
    /// names it compares exactly.
    fn member_key(slot: Slot, member: &str) -> String {
        match slot {
            Slot::Method => member.to_lowercase(),
            Slot::Property | Slot::Constant => member.to_string(),
        }
    }

    /// Walks a class, its base class, its interfaces and its traits.
    ///
    /// The visited set is not an optimization: PHP rejects a cycle, but a
    /// half-written file can still describe one and this must not hang.
    fn in_hierarchy<T>(&self, class: &str, probe: impl Fn(&str) -> Option<T>) -> Option<T> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut stack = vec![class.to_string()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(found) = probe(&current) {
                return Some(found);
            }
            if let Some(parents) = self.parents.get(&current) {
                stack.extend(parents.iter().cloned());
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

    /// The answer for a name written in an expression or a type.
    ///
    /// Never None: a name that can be read is a name that can be reported, and
    /// an EXTERNAL_SYMBOL keeps the edge the occurrence stands for.
    fn name_answer(&self, name: &str, doc: u32, namespace: &str, kind: NameKind) -> Answer {
        let candidates = self.candidates(name, doc, namespace, kind);
        for candidate in &candidates.names {
            // The order is the one the syntax implies: a call names a function
            // first, a type a class, a bare read anything at all.
            let found = match kind {
                NameKind::Class => self.declared_class(candidate),
                NameKind::Function => self
                    .declared_function(candidate)
                    .or_else(|| self.declared_class(candidate)),
                NameKind::Value => self
                    .declared_class(candidate)
                    .or_else(|| self.declared_constant(candidate))
                    .or_else(|| self.declared_function(candidate)),
            };
            if let Some(site) = found {
                return Answer::Internal(site);
            }
        }
        // A class the tables miss may still be reachable: through PSR-4, or —
        // in a value position — through a `use` alias the constant map does not
        // hold.
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
                        let mut inner = DeclScope { namespace: name, class: scope.class.clone() };
                        self.walk_decl_children(doc, source, body, &mut inner);
                    }
                    // The statement form runs to the next one or to the end of
                    // the file, so it rewrites what its siblings see.
                    None => scope.namespace = name,
                }
            }
            "namespace_use_declaration" => self.index_use_declaration(doc, source, node),
            "class_declaration" | "interface_declaration" | "trait_declaration"
            | "enum_declaration" => self.declare_class(doc, source, node, scope),
            "method_declaration" => {
                if let Some(class) = scope.class.clone() {
                    let namespace = scope.namespace.clone();
                    self.declare_method(doc, source, node, &class, &namespace);
                }
                self.walk_decl_children(doc, source, node, scope);
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
                self.walk_decl_children(doc, source, node, scope);
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
                            self.push_pending(doc, source, &class, None, child, &namespace);
                        }
                    }
                }
            }
            _ => self.walk_decl_children(doc, source, node, scope),
        }
    }

    fn push_pending(
        &mut self,
        doc: u32,
        source: &[u8],
        class: &str,
        property: Option<&str>,
        reference: TsNode<'_>,
        namespace: &str,
    ) {
        self.pending.push(Pending {
            class: class.to_lowercase(),
            property: property.map(str::to_string),
            name: ts::text(reference, source).into_owned(),
            doc,
            namespace: namespace.to_string(),
        });
    }

    fn declare_class(&mut self, doc: u32, source: &[u8], node: TsNode<'_>, scope: &DeclScope) {
        // An anonymous class is deliberately not indexed: nothing outside the
        // expression that creates it can name it.
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

        // `extends` and `implements` only: an enum's backing type is a
        // positional `primitive_type` sharing the child list with them, and a
        // scan of the whole list would report `string` as an interface.
        for clause in ts::children(node) {
            if !matches!(clause.kind(), "base_clause" | "class_interface_clause") {
                continue;
            }
            for reference in class_refs(clause) {
                self.push_pending(doc, source, &fqn, None, reference, &scope.namespace);
            }
        }

        if let Some(body) = node.child_by_field_name("body") {
            let mut inner = DeclScope {
                namespace: scope.namespace.clone(),
                class: Some(fqn),
            };
            self.walk_decl_children(doc, source, body, &mut inner);
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
            .entry((class.to_lowercase(), name))
            .or_insert(Self::site(doc, name_node));

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
                self.push_pending(doc, source, class, Some(&name), only, namespace);
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
            let class = self.class_ref(&item.name, item.doc, &item.namespace);
            match item.property {
                Some(property) => {
                    self.property_types.insert((item.class, property), class);
                }
                // A base class outside the workspace contributes no members
                // this index could find, so the link is not worth keeping.
                None => {
                    if let ClassRef::Declared(parent) = class {
                        self.parents.entry(item.class).or_default().push(parent);
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
    fn record_type(&mut self, doc: u32, source: &[u8], type_node: TsNode<'_>, namespace: &str) {
        for reference in class_refs(type_node) {
            self.record_name(doc, source, reference, namespace, NameKind::Class);
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
                scope.vars.get(&name).cloned()
            }
            // `self`, `static`, `parent`.
            "relative_scope" => match ts::text(node, source).to_lowercase().as_str() {
                "parent" => scope.parent.clone(),
                _ => scope.class.clone(),
            },
            "name" | "qualified_name" | "relative_name" => {
                Some(self.class_ref(&ts::text(node, source), doc, &scope.namespace))
            }
            "object_creation_expression" => {
                let class =
                    ts::child_of_types(node, &["name", "qualified_name", "relative_name"])?;
                Some(self.class_ref(&ts::text(class, source), doc, &scope.namespace))
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
        let namespace = scope.namespace.clone();
        for parameter in ts::children(parameters) {
            if !matches!(
                parameter.kind(),
                "simple_parameter" | "variadic_parameter" | "property_promotion_parameter"
            ) {
                continue;
            }
            if let Some(type_node) = parameter.child_by_field_name("type") {
                self.record_type(doc, source, type_node, &namespace);
                let references = class_refs(type_node);
                // One class names a receiver; a union names none.
                if let [only] = references[..]
                    && let Some(ident) = parameter
                        .child_by_field_name("name")
                        .and_then(variable_identifier)
                {
                    let class = self.class_ref(&ts::text(only, source), doc, &namespace);
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
        let namespace = inner.namespace.clone();
        if let Some(attributes) = node.child_by_field_name("attributes") {
            self.walk_use_children(doc, source, attributes, &mut inner);
        }
        if let Some(parameters) = node.child_by_field_name("parameters") {
            self.bind_parameters(doc, source, parameters, &mut inner);
        }
        if let Some(return_type) = node.child_by_field_name("return_type") {
            self.record_type(doc, source, return_type, &namespace);
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

    fn walk_uses(&mut self, doc: u32, source: &[u8], node: TsNode<'_>, scope: &mut UseScope) {
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
            // The imported paths are not use-sites: the visitor turns them into
            // IMPORT nodes itself and records no occurrence for them.
            "namespace_use_declaration" => {}

            "class_declaration" | "interface_declaration" | "trait_declaration"
            | "enum_declaration" => self.walk_class(doc, source, node, scope),

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
                        // Calling the result of an expression: `$factory()`.
                        _ => self.walk_uses(doc, source, function, scope),
                    }
                }
                if let Some(arguments) = node.child_by_field_name("arguments") {
                    self.walk_uses(doc, source, arguments, scope);
                }
            }
            "object_creation_expression" => {
                let namespace = scope.namespace.clone();
                for child in ts::children(node) {
                    match child.kind() {
                        "name" | "qualified_name" | "relative_name" => {
                            self.record_name(doc, source, child, &namespace, NameKind::Class);
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
                self.walk_scoped(doc, source, node, scope, Slot::Method);
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
                    let namespace = scope.namespace.clone();
                    self.record_name(doc, source, operand, &namespace, NameKind::Class);
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
        let namespace = scope.namespace.clone();
        if let Some(type_node) = node.child_by_field_name("type") {
            self.record_type(doc, source, type_node, &namespace);
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

        // Only the first `extends` is what `parent::` means; an interface is
        // not, and neither is a trait.
        let mut parent = None;
        for clause in ts::children(node) {
            if !matches!(clause.kind(), "base_clause" | "class_interface_clause") {
                continue;
            }
            for reference in class_refs(clause) {
                if clause.kind() == "base_clause" && parent.is_none() {
                    parent = Some(self.class_ref(&ts::text(reference, source), doc, &namespace));
                }
                self.record_name(doc, source, reference, &namespace, NameKind::Class);
            }
        }

        let mut inner = UseScope { namespace, class, parent, vars: HashMap::new() };
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
            self.walk_uses(doc, source, owner, scope);
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
        self.walk_uses(doc, source, qualifier, scope);
    }

    /// `catch (FooError | BarError $e) { … }` — a type position, and one that
    /// types the variable it binds.
    fn walk_catch(&mut self, doc: u32, source: &[u8], node: TsNode<'_>, scope: &mut UseScope) {
        let namespace = scope.namespace.clone();
        if let Some(type_node) = node.child_by_field_name("type") {
            self.record_type(doc, source, type_node, &namespace);
            let references = class_refs(type_node);
            // A union of exception types names no single class.
            if let [only] = references[..]
                && let Some(ident) = node
                    .child_by_field_name("name")
                    .and_then(variable_identifier)
            {
                let class = self.class_ref(&ts::text(only, source), doc, &namespace);
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
            let mut scope = DeclScope { namespace: String::new(), class: None };
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
