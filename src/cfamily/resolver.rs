//! Symbol resolution for C and C++, without a compiler.
//!
//! Every precise option for this family — `scip-clang`, clangd's index,
//! libclang — hard-requires a `compile_commands.json`, and that file is the
//! problem. It cannot be derived from sources; it is a by-product of running
//! the build. Surveying twelve of the most prominent C and C++ repositories
//! found it checked in **none** of them, and both redis and the Linux kernel
//! gitignore it outright. A resolver built on it would report `unavailable` on
//! very nearly every folder a user points callix at, which is a worse failure
//! than Go's or Rust's "use the toolchain a project of that language already
//! has": there, the requirement is something the project necessarily owns.
//!
//! So the shipped default is this — a symbol table over the sources callix
//! already parsed, reporting [`ResolverStatus::Degraded`]. It is not a
//! placeholder. On a codebase with headers and definitions in the same tree it
//! answers the great majority of call and reference sites, because C and C++
//! resolve names by declaration visibility and that is exactly what a header
//! graph encodes.
//!
//! What it deliberately does not do:
//!
//! - **Evaluate the preprocessor.** Every arm of an `#if` is in the graph, so
//!   two mutually exclusive definitions of one function both exist. A
//!   compiler-based indexer sees one. Which is right depends on why you are
//!   asking; this way nothing is invisible because of a flag nobody recorded.
//! - **Run the preprocessor.** The visitor expands the macros of the project where
//!   they are invoked, and the table sees the result as ordinary calls and
//!   declarations; a macro from a system header is unknown, and conditionals are
//!   not evaluated.
//! - **Overload resolution.** A C++ call site names a function; picking which
//!   overload it selects needs the argument types, which needs a type checker.
//!   The call binds to the single candidate when there is one and to the first
//!   in declaration order when there are several, and says so in the edge.
//!
//! A compiler-backed backend plugs in through the adapters' `resolver=`
//! argument, and `src/rustlang/scip.rs` already holds a decoder that does not
//! care which indexer produced the index. Note the asymmetry: a custom resolver
//! is asked "what is defined at this position?", the way every other adapter
//! asks its backend, while the table below answers by name — a compdb-derived
//! index has positions, and this one cannot. Selection stays explicit rather
//! than detected, so that the same folder cannot yield structurally different
//! graphs depending on whether `cmake` happened to have run.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::py::prelude::*;
use crate::py::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::node::NodeKind;
use super::visitor::CallSite;
use crate::status::ResolverStatus;

/// The built-in backend, as the adapter's `ResolverSlot` records it.
///
/// A marker with no state, because unlike the other four backends this one
/// cannot exist before analysis: [`CFamilyIndex`] is keyed by name and assembled
/// from the declarations and includes the survey pass found, none of which exist
/// until the files have been read. The slot records only *that* the table is in
/// use, and `analyze` builds it.
///
/// It deliberately does not implement `NativeResolver`: that trait's
/// `resolve(path, line, col)` asks "what is defined at this position?", which is
/// the question this family answers in the other direction. A custom resolver
/// passed to `CAdapter` or `CppAdapter` *is* asked that question, because a
/// caller who has a compiler-backed index has positions to answer with.
pub struct SymbolTable;

/// One name a translation unit can see.
#[derive(Clone)]
struct Symbol {
    qualified_name: String,
    kind: NodeKind,
    /// Project-relative file the node was built from.
    file_rel: String,
    /// A file-local symbol is visible only inside `file_rel`; an external one is
    /// visible wherever it has been declared.
    internal: bool,
    /// For a file-scope variable: the class-like type it was declared with.
    value_type: Option<String>,
}

/// Definitions callix parsed, indexed by the shapes a use-site can ask for.
///
/// Not a `#[pyclass]` with `prepare`/`resolve_all`: the C family resolves from
/// the graph the adapter has just built rather than from an external index, so
/// there is nothing to prepare and no subprocess to drive. The adapter calls
/// [`CFamilyIndex::lookup`] directly. [`CFamilyResolver`] below is the
/// Python-visible face, kept so the family reports status and metrics the same
/// way the other four do.
#[derive(Default)]
pub struct CFamilyIndex {
    /// Bare name → every symbol with that name.
    by_name: HashMap<String, Vec<Symbol>>,
    /// Which headers a file can see, transitively through `#include`.
    visible: HashMap<String, HashSet<String>>,
    /// Qualified class name → the base classes as written.
    classes: BTreeMap<String, Vec<String>>,
    /// (qualified class, member name) → every member of that name, constructors
    /// included, whose parameters tell overloads apart.
    members: HashMap<(String, String), Vec<Member>>,
    /// (qualified class, field name) → the class-like type the field was declared with.
    fields: HashMap<(String, String), String>,
    /// (qualified class, field name) → the field's qualified name: what a call
    /// through a function-pointer member lands on.
    field_names: HashMap<(String, String), String>,
    /// `namespace short = a::b;`: alias → the namespace it stands for.
    namespace_aliases: HashMap<String, String>,
    /// Qualified typedef / `using` name → the class-like type it stands for.
    aliases: BTreeMap<String, String>,
    /// Qualified names of the fields and file-scope variables that hold an object
    /// itself (not a pointer, array or function): the only ones `a = b` can call an
    /// operator on.
    objects: HashSet<String>,
}

/// A member function of a class.
#[derive(Clone)]
struct Member {
    qualified_name: String,
    kind: NodeKind,
}

/// What a use-site that names a function resolved to.
pub enum Outcome {
    /// A declaration of the project; the flag says several fitted and the first won.
    Found { qualified_name: String, kind: NodeKind, ambiguous: bool },
    /// Not the table's question: a read, an annotation, a field in an initialiser list.
    NotACall,
    /// The site names nothing the project declares (a library type, a built-in).
    Outside,
}

/// What a member access's receiver turned out to be.
enum Receiver {
    /// Nothing in reach says what it is.
    Unknown,
    /// The classes it can be; empty means a type the project does not declare.
    Classes(Vec<String>),
}

impl CFamilyIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a definition the graph holds.
    ///
    /// `qualified_name` is what the visitor built, so for C++ a callable's
    /// entry already carries its normalized parameter list; the bare name is
    /// what a call site can be matched on, and is indexed separately.
    // The adapter declares through `declare_typed`; this stays for the tests.
    #[allow(dead_code)]
    pub fn declare(
        &mut self,
        bare_name: &str,
        qualified_name: &str,
        kind: NodeKind,
        file_rel: &str,
        internal: bool,
    ) {
        self.declare_typed(bare_name, qualified_name, kind, file_rel, internal, None);
    }

    /// [`CFamilyIndex::declare`] for a variable whose declared type is known.
    pub fn declare_typed(
        &mut self,
        bare_name: &str,
        qualified_name: &str,
        kind: NodeKind,
        file_rel: &str,
        internal: bool,
        value_type: Option<String>,
    ) {
        if bare_name.is_empty() {
            return;
        }
        self.by_name.entry(bare_name.to_string()).or_default().push(Symbol {
            qualified_name: qualified_name.to_string(),
            kind,
            file_rel: file_rel.to_string(),
            internal,
            value_type,
        });
    }

    /// Records a typedef or `using` alias and the type it stands for.
    pub fn declare_alias(&mut self, qualified_name: &str, type_text: &str) {
        if let Some(key) = type_key(type_text) {
            self.aliases.entry(qualified_name.to_string()).or_insert(key);
        }
    }

    /// Records that `file_rel` includes `included`, directly.
    pub fn include(&mut self, file_rel: &str, included: &str) {
        self.visible
            .entry(file_rel.to_string())
            .or_default()
            .insert(included.to_string());
    }

    /// Closes the include relation transitively.
    ///
    /// A header that includes another makes the second's declarations visible
    /// to everything that includes the first, which is what makes a two-hop
    /// include chain resolve. Iterating to a fixed point rather than recursing
    /// keeps a cyclic include — legal, and common before `#pragma once` was
    /// universal — from being a stack overflow.
    pub fn close_includes(&mut self) {
        loop {
            let mut grew = false;
            let files: Vec<String> = self.visible.keys().cloned().collect();
            for file in files {
                let direct: Vec<String> =
                    self.visible.get(&file).map(|s| s.iter().cloned().collect()).unwrap_or_default();
                let mut additions: Vec<String> = Vec::new();
                for header in &direct {
                    if let Some(inner) = self.visible.get(header) {
                        for deeper in inner {
                            if deeper != &file && !direct.contains(deeper) {
                                additions.push(deeper.clone());
                            }
                        }
                    }
                }
                if additions.is_empty() {
                    continue;
                }
                let entry = self.visible.entry(file).or_default();
                for addition in additions {
                    grew |= entry.insert(addition);
                }
            }
            if !grew {
                return;
            }
        }
    }

    /// Whether a symbol declared in `owner` is visible from `from`.
    fn reachable(&self, from: &str, owner: &str) -> bool {
        from == owner
            || self.visible.get(from).is_some_and(|headers| headers.contains(owner))
    }

    /// The definition a use-site names, if exactly one is a credible answer.
    ///
    /// Returns the qualified name, the kind, and whether the answer was
    /// ambiguous — several visible candidates, which is where overload
    /// resolution would be needed and is not available.
    // The adapter asks `lookup_kinds`; this stays for the tests.
    #[allow(dead_code)]
    pub fn lookup(&self, name: &str, from_file: &str) -> Option<(String, NodeKind, bool)> {
        self.lookup_kinds(name, from_file, |_| true)
    }

    /// [`CFamilyIndex::lookup`] restricted to the kinds `accept` allows. The filter
    /// runs BEFORE the first candidate is chosen, so a field declared earlier can no
    /// longer hide a function of the same name.
    pub fn lookup_kinds(
        &self,
        name: &str,
        from_file: &str,
        accept: impl Fn(NodeKind) -> bool,
    ) -> Option<(String, NodeKind, bool)> {
        let visible = self.visible(name, from_file, &accept);
        // Nothing reachable was declared. Falling back to an unreachable
        // candidate would be guessing: `#include` visibility is the language's
        // own rule for which name a translation unit means.
        let chosen = visible.first()?;
        Some((chosen.qualified_name.clone(), chosen.kind, visible.len() > 1))
    }

    /// The symbols named `name` of an accepted kind that `from_file` can see. A
    /// file-local symbol wins inside its own file even when an external-linkage
    /// symbol of the same name exists elsewhere: that is what `static` means.
    fn visible(&self, name: &str, from_file: &str, accept: &dyn Fn(NodeKind) -> bool) -> Vec<&Symbol> {
        let Some(candidates) = self.by_name.get(name) else {
            return Vec::new();
        };
        let fits: Vec<&Symbol> = candidates.iter().filter(|s| accept(s.kind)).collect();
        let local: Vec<&Symbol> = fits
            .iter()
            .copied()
            .filter(|s| s.internal && self.reachable(from_file, &s.file_rel))
            .collect();
        if local.is_empty() {
            fits.into_iter().filter(|s| !s.internal && self.reachable(from_file, &s.file_rel)).collect()
        } else {
            local
        }
    }

    /// A written scope with a namespace alias in its first component expanded.
    fn expand_qualifier(&self, qualifier: &[String]) -> Vec<String> {
        match qualifier.first().and_then(|first| self.namespace_aliases.get(first)) {
            Some(target) => {
                let mut expanded: Vec<String> =
                    target.split("::").filter(|c| !c.is_empty()).map(str::to_string).collect();
                expanded.extend(qualifier.iter().skip(1).cloned());
                expanded
            }
            None => qualifier.to_vec(),
        }
    }

    /// The CLASS or TYPE_ALIAS a type name written in `from_file` means.
    ///
    /// Only types are candidates: a function or a variable of the same name is not
    /// what a type position names. A written scope must match the candidate's
    /// qualified name (`std::string` never binds a project `mine::string`; there is
    /// no fallback to the bare name). Visibility is the same as `lookup`. An
    /// unadorned name prefers the typedef (C's `typedef struct S {..} S`), a name
    /// written after `struct`/`union`/`enum`/`class` (`tag`) prefers the class. The
    /// flag says several candidates of the chosen kind remained.
    pub fn resolve_type(
        &self,
        name: &str,
        qualifier: &[String],
        tag: bool,
        from_file: &str,
    ) -> Option<(String, NodeKind, bool)> {
        let candidates = self.by_name.get(name)?;
        let qualifier = self.expand_qualifier(qualifier);
        let exact = format!("{}::{name}", qualifier.join("::"));
        let tail = format!("::{exact}");
        let fits: Vec<&Symbol> = candidates
            .iter()
            .filter(|s| matches!(s.kind, NodeKind::Class | NodeKind::TypeAlias))
            .filter(|s| qualifier.is_empty() || s.qualified_name == exact || s.qualified_name.ends_with(&tail))
            .collect();
        let local: Vec<&Symbol> = fits
            .iter()
            .copied()
            .filter(|s| s.internal && self.reachable(from_file, &s.file_rel))
            .collect();
        let visible: Vec<&Symbol> = if local.is_empty() {
            fits.iter()
                .copied()
                .filter(|s| !s.internal && self.reachable(from_file, &s.file_rel))
                .collect()
        } else {
            local
        };
        let order = if tag {
            [NodeKind::Class, NodeKind::TypeAlias]
        } else {
            [NodeKind::TypeAlias, NodeKind::Class]
        };
        for kind in order {
            let of_kind: Vec<&&Symbol> = visible.iter().filter(|s| s.kind == kind).collect();
            if let Some(first) = of_kind.first() {
                return Some((first.qualified_name.clone(), kind, of_kind.len() > 1));
            }
        }
        None
    }

    // -- classes and their members ----------------------------------------------

    /// Records a class the project declares.
    pub fn declare_class(&mut self, qualified_name: &str) {
        self.classes.entry(qualified_name.to_string()).or_default();
    }

    /// Records a base class of `class`, as written.
    pub fn add_base(&mut self, class: &str, base: &str) {
        let bases = self.classes.entry(class.to_string()).or_default();
        if !bases.iter().any(|b| b == base) {
            bases.push(base.to_string());
        }
    }

    /// Records a member function or constructor of `class`.
    pub fn declare_member(&mut self, class: &str, name: &str, qualified_name: &str, kind: NodeKind) {
        let entry = self.members.entry((class.to_string(), name.to_string())).or_default();
        if !entry.iter().any(|m| m.qualified_name == qualified_name) {
            entry.push(Member { qualified_name: qualified_name.to_string(), kind });
        }
    }

    /// Records `namespace alias = target;`. Aliases are global to the table, not
    /// scoped to the block that declared them: two blocks naming different
    /// namespaces the same way is rare enough that the first one wins.
    pub fn declare_namespace_alias(&mut self, alias: &str, target: &str) {
        self.namespace_aliases.entry(alias.to_string()).or_insert_with(|| target.to_string());
    }

    /// Records the type a field of `class` was declared with.
    pub fn declare_field(&mut self, class: &str, field: &str, qualified_name: &str, type_text: &str) {
        self.field_names.insert((class.to_string(), field.to_string()), qualified_name.to_string());
        if let Some(key) = type_key(type_text) {
            self.fields.insert((class.to_string(), field.to_string()), key);
        }
    }

    /// Records that a field or file-scope variable holds an object itself.
    pub fn declare_object(&mut self, qualified_name: &str) {
        self.objects.insert(qualified_name.to_string());
    }

    /// The field `name` of `class` or one of its bases, when there is one.
    fn field_of(&self, class: &str, name: &str) -> Option<String> {
        self.lineage(class).into_iter().find_map(|c| self.field_names.get(&(c, name.to_string())).cloned())
    }

    /// The classes a type as written can mean, sorted: the written name is the
    /// whole qualified name or its tail (`Shape` for `n::Shape`).
    fn classes_named(&self, written: &str) -> Vec<&String> {
        let tail = format!("::{written}");
        self.classes.keys().filter(|q| q.as_str() == written || q.ends_with(&tail)).collect()
    }

    /// The class itself and its bases, nearest first, each once.
    fn lineage(&self, class: &str) -> Vec<String> {
        let mut order = vec![class.to_string()];
        let mut index = 0;
        while index < order.len() && order.len() < 64 {
            let current = order[index].clone();
            for base in self.classes.get(&current).into_iter().flatten() {
                if let Some(found) = self.classes_named(base).first()
                    && !order.contains(found)
                {
                    order.push((*found).clone());
                }
            }
            index += 1;
        }
        order
    }

    /// The member `name` of `class`: the nearest class in its lineage that has one
    /// of that name hides the rest, as C++ name lookup does.
    fn member_of(&self, class: &str, name: &str, site: &CallSite) -> Option<(Member, bool)> {
        for candidate in self.lineage(class) {
            if let Some(found) = self.members.get(&(candidate, name.to_string())) {
                return Some(pick(found, |m| m.qualified_name.clone(), site));
            }
        }
        None
    }

    /// The type of a field of `class` or of one of its bases.
    fn field_type(&self, class: &str, field: &str) -> Option<String> {
        self.lineage(class).into_iter().find_map(|c| self.fields.get(&(c, field.to_string())).cloned())
    }

    /// The classes a type name as written can mean: the class itself, or the class a
    /// typedef / `using` alias leads to (at most 8 hops). Empty for a library type.
    fn class_of(&self, written: &str) -> Vec<String> {
        let mut current = written.to_string();
        for _ in 0..8 {
            let found = self.classes_named(&current);
            if !found.is_empty() {
                return found.into_iter().cloned().collect();
            }
            let tail = format!("::{current}");
            let next = self
                .aliases
                .iter()
                .find(|(q, _)| q.as_str() == current || q.ends_with(&tail))
                .map(|(_, target)| target.clone());
            match next {
                Some(target) if target != current => current = target,
                _ => return Vec::new(),
            }
        }
        Vec::new()
    }

    /// The declared type of a visible file-scope variable.
    fn global_type(&self, name: &str, from_file: &str) -> Option<String> {
        self.visible(name, from_file, &|k| k == NodeKind::Variable)
            .first()
            .and_then(|s| s.value_type.clone())
    }

    /// What the receiver of a member access is: its root type (a typed local, `this`,
    /// a field of the enclosing class, a global), then one step per field of
    /// `receiver_path`.
    fn receiver_classes(&self, site: &CallSite, from_file: &str) -> Receiver {
        let start = site.receiver_type.clone().or_else(|| {
            let name = site.receiver_name.as_deref()?;
            site.class_scope
                .as_deref()
                .and_then(|scope| self.field_type(scope, name))
                .or_else(|| self.global_type(name, from_file))
        });
        let Some(mut current) = start else {
            return Receiver::Unknown;
        };
        for step in &site.receiver_path {
            let classes = self.class_of(&current);
            if classes.is_empty() {
                return Receiver::Classes(Vec::new());
            }
            match classes.iter().find_map(|c| self.field_type(c, step)) {
                Some(next) => current = next,
                None => return Receiver::Unknown,
            }
        }
        Receiver::Classes(self.class_of(&current))
    }

    /// The classes the LEFT operand of an operator can be. Like `receiver_classes`, but a
    /// field or global that is not recorded in `objects` (a pointer, an array) gives
    /// nothing, since `p = q` on pointers is built in.
    fn operand_classes(&self, site: &CallSite, from_file: &str) -> Vec<String> {
        let mut holder: Option<String> = None;
        let start = if let Some(t) = site.receiver_type.clone() {
            Some(t)
        } else if let Some(name) = site.receiver_name.as_deref() {
            if let Some(scope) = site.class_scope.as_deref()
                && let Some(t) = self.field_type(scope, name)
            {
                holder = self.field_of(scope, name);
                Some(t)
            } else {
                let visible = self.visible(name, from_file, &|k| k == NodeKind::Variable);
                match visible.first() {
                    Some(symbol) => {
                        holder = Some(symbol.qualified_name.clone());
                        symbol.value_type.clone()
                    }
                    None => None,
                }
            }
        } else {
            None
        };
        let Some(mut current) = start else {
            return Vec::new();
        };
        for step in &site.receiver_path {
            let classes = self.class_of(&current);
            let next = classes
                .iter()
                .find_map(|c| self.field_type(c, step).map(|t| (t, self.field_of(c, step))));
            match next {
                Some((t, h)) => {
                    current = t;
                    holder = h;
                }
                None => return Vec::new(),
            }
        }
        if let Some(h) = &holder
            && !self.objects.contains(h)
        {
            return Vec::new();
        }
        self.class_of(&current)
    }

    /// An overloaded operator use. The left operand's class (and its bases) first, as a
    /// member; then, except for `=`, `[]` and `->` which are member-only, a visible
    /// two-parameter namespace-scope function, admitted only when an operand's class
    /// appears in the lineage of the class a parameter names. Never a bare-name fallback.
    fn resolve_operator(&self, name: &str, site: &CallSite, from_file: &str) -> Outcome {
        let lefts = self.operand_classes(site, from_file);
        for class in &lefts {
            if let Some((member, ambiguous)) = self.member_of(class, name, site) {
                return Outcome::Found {
                    qualified_name: member.qualified_name,
                    kind: member.kind,
                    ambiguous: ambiguous || lefts.len() > 1,
                };
            }
        }
        if matches!(name, "operator=" | "operator[]" | "operator->") {
            return Outcome::NotACall;
        }
        let right_kind = site.arg_kinds.first().cloned().unwrap_or_else(|| "?".to_string());
        let right_name = right_kind.trim_start_matches("&&");
        let right_classes = if right_name.is_empty() || right_name == "?" {
            Vec::new()
        } else {
            self.class_of(right_name)
        };
        let candidates: Vec<&Symbol> = self
            .visible(name, from_file, &|k| k == NodeKind::Function)
            .into_iter()
            .filter(|symbol| {
                let params = params_of(&symbol.qualified_name);
                if params.len() != 2 {
                    return false;
                }
                let evidence = |operands: &[String], parameter: &str| {
                    let Some(key) = type_key(parameter) else {
                        return false;
                    };
                    let wanted = self.class_of(&key);
                    operands.iter().any(|operand| self.lineage(operand).iter().any(|c| wanted.contains(c)))
                };
                evidence(&lefts, params[0].as_str()) || evidence(&right_classes, params[1].as_str())
            })
            .collect();
        if candidates.is_empty() {
            return Outcome::NotACall;
        }
        let left_kind = lefts
            .first()
            .map(|c| c.rsplit("::").next().unwrap_or(c.as_str()).to_string())
            .unwrap_or_else(|| "?".to_string());
        let mut probe = site.clone();
        probe.arity = Some(2);
        probe.arg_kinds = vec![left_kind, right_kind];
        let (symbol, tie) = pick(&candidates, |s| s.qualified_name.clone(), &probe);
        Outcome::Found {
            qualified_name: symbol.qualified_name.clone(),
            kind: symbol.kind,
            ambiguous: tie || lefts.len() > 1,
        }
    }

    /// Resolves a call or a construction using everything the site says.
    ///
    /// The order is the order C++ reads a name in: a receiver's class decides a
    /// member call; a written scope names a class or a namespace; an unqualified
    /// name in a method means the class first. Only a receiver whose type is not
    /// known falls back to the bare name - and then the edge says `ambiguous` when
    /// the name is not unique.
    pub fn resolve(&self, name: &str, role: &str, site: &CallSite, from_file: &str) -> Outcome {
        // A namespace alias in the written scope stands for what it names.
        let expanded;
        let site = match site.qualifier.first().and_then(|first| self.namespace_aliases.get(first)) {
            Some(target) => {
                let mut qualifier: Vec<String> = target.split("::").filter(|c| !c.is_empty()).map(str::to_string).collect();
                qualifier.extend(site.qualifier.iter().skip(1).cloned());
                expanded = CallSite { qualifier, ..site.clone() };
                &expanded
            }
            None => site,
        };
        let written = if site.qualifier.is_empty() {
            name.to_string()
        } else {
            format!("{}::{name}", site.qualifier.join("::"))
        };

        // A construction: `T x;`, `new T(1)`, a base in an initialiser list.
        let base_init = site.in_initializer_list
            && site
                .class_scope
                .as_deref()
                .is_some_and(|scope| self.classes.get(scope).is_some_and(|bases| bases.iter().any(|b| b == name)));
        if site.constructs || base_init {
            let classes = self.classes_named(&written);
            let Some(class) = classes.first() else {
                return Outcome::Outside;
            };
            let bare = class.rsplit("::").next().unwrap_or(class);
            if let Some(constructors) = self.members.get(&((*class).clone(), bare.to_string())) {
                let (member, mut ambiguous) = pick(constructors, |m| m.qualified_name.clone(), site);
                ambiguous |= classes.len() > 1;
                return Outcome::Found { qualified_name: member.qualified_name, kind: member.kind, ambiguous };
            }
            // No constructor of its own: the instantiation is of the class.
            return Outcome::Found {
                qualified_name: (*class).clone(),
                kind: NodeKind::Class,
                ambiguous: classes.len() > 1,
            };
        }
        if site.in_initializer_list {
            // `Set() : next_(2)`: a field of the class, bases included. Anything else
            // (a delegating constructor) is not the table's question.
            if let Some(scope) = site.class_scope.as_deref()
                && let Some(field) = self.field_of(scope, name)
            {
                return Outcome::Found { qualified_name: field, kind: NodeKind::Attribute, ambiguous: false };
            }
            return Outcome::NotACall;
        }

        // An overloaded operator: never the bare-name fallback of a member access.
        if site.overloaded_operator {
            return self.resolve_operator(name, site, from_file);
        }

        // A member access (call, read or write): the receiver's declared type decides.
        if site.member || site.receiver_type.is_some() || site.receiver_name.is_some() {
            let receiver = self.receiver_classes(site, from_file);
            if role == "call" {
                return match receiver {
                    Receiver::Classes(classes) => {
                        for class in &classes {
                            if let Some((member, ambiguous)) = self.member_of(class, name, site) {
                                return Outcome::Found {
                                    qualified_name: member.qualified_name,
                                    kind: member.kind,
                                    ambiguous,
                                };
                            }
                            // A call through a member that holds a function pointer.
                            if let Some(field) = self.field_of(class, name) {
                                return Outcome::Found { qualified_name: field, kind: NodeKind::Attribute, ambiguous: false };
                            }
                        }
                        if site.speculative { Outcome::NotACall } else { Outcome::Outside }
                    }
                    // `obj.f()` with an `obj` nothing in reach declares: any method or
                    // function-pointer field `f` fits; `x->f()` is never a free function.
                    Receiver::Unknown => self.by_bare_name(name, site, from_file, &|k| {
                        matches!(k, NodeKind::Method | NodeKind::Attribute)
                    }),
                };
            }
            return match receiver {
                Receiver::Classes(classes) => {
                    for class in &classes {
                        if let Some(field) = self.field_of(class, name) {
                            return Outcome::Found { qualified_name: field, kind: NodeKind::Attribute, ambiguous: false };
                        }
                        if let Some((member, ambiguous)) = self.member_of(class, name, site) {
                            return Outcome::Found { qualified_name: member.qualified_name, kind: member.kind, ambiguous };
                        }
                    }
                    // The type is known and has no such field: not a project symbol.
                    Outcome::Outside
                }
                Receiver::Unknown => {
                    self.by_bare_name(name, site, from_file, &|k| k == NodeKind::Attribute)
                }
            };
        }

        // A written scope: a class's member, or a namespace's function.
        if !site.qualifier.is_empty() {
            let scope = site.qualifier.join("::");
            for class in self.classes_named(&scope) {
                if let Some((member, ambiguous)) = self.member_of(class, name, site) {
                    return Outcome::Found { qualified_name: member.qualified_name, kind: member.kind, ambiguous };
                }
            }
            let tail = format!("::{scope}::{name}");
            let in_scope: Vec<&Symbol> = self
                .by_name
                .get(name)
                .into_iter()
                .flatten()
                .filter(|s| {
                    let bare = s.qualified_name.split('(').next().unwrap_or(&s.qualified_name);
                    let full = format!("{scope}::{name}");
                    (bare == full || bare.ends_with(&tail)) && (!s.internal || self.reachable(from_file, &s.file_rel))
                })
                .collect();
            if !in_scope.is_empty() {
                let (symbol, ambiguous) = pick(&in_scope, |s| s.qualified_name.clone(), site);
                return Outcome::Found { qualified_name: symbol.qualified_name.clone(), kind: symbol.kind, ambiguous };
            }
            return Outcome::Outside;
        }

        // An unqualified name inside a method means the class first.
        if let Some(scope) = site.class_scope.as_deref() {
            if let Some((member, ambiguous)) = self.member_of(scope, name, site) {
                return Outcome::Found { qualified_name: member.qualified_name, kind: member.kind, ambiguous };
            }
            // A member function pointer called by its bare name: `cb_()`.
            if let Some(field) = self.field_of(scope, name) {
                return Outcome::Found { qualified_name: field, kind: NodeKind::Attribute, ambiguous: false };
            }
        }
        // A field is never the answer for an unqualified name.
        match self.by_bare_name(name, site, from_file, &|k| k != NodeKind::Attribute) {
            Outcome::Outside if site.speculative => Outcome::NotACall,
            found => found,
        }
    }

    /// The old question: a name, and what a file can see by that name. Overloads
    /// are told apart by the call's arguments when there are several.
    fn by_bare_name(
        &self,
        name: &str,
        site: &CallSite,
        from_file: &str,
        accept: &dyn Fn(NodeKind) -> bool,
    ) -> Outcome {
        let visible = self.visible(name, from_file, accept);
        if visible.is_empty() {
            return Outcome::Outside;
        }
        let (symbol, ambiguous) = pick(&visible, |s| s.qualified_name.clone(), site);
        Outcome::Found { qualified_name: symbol.qualified_name.clone(), kind: symbol.kind, ambiguous }
    }

}

/// The Python-visible resolver for C and C++.
///
/// Exposed for symmetry with the other four adapters and so a caller can ask
/// what it is: `status()` answers `DEGRADED`, always, because a symbol table is
/// not a type checker and reporting `OK` would misrepresent the graph. The
/// resolution itself happens inside `analyze`, against the index built from the
/// sources it has already parsed, so there is no `prepare` step to drive from
/// Python and nothing to configure.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct CFamilyResolver;

#[gen_stub_pymethods]
#[pymethods]
impl CFamilyResolver {
    #[new]
    fn new() -> Self {
        Self
    }

    /// Always `DEGRADED`: a symbol table over the analysed sources, with no
    /// preprocessor evaluation, no macro expansion and no overload resolution.
    fn status(&self) -> ResolverStatus {
        ResolverStatus::Degraded
    }

    fn __repr__(&self) -> String {
        "CFamilyResolver(status=degraded)".to_string()
    }
}

/// The class-like name a declared type spells: qualifiers, pointers, references
/// and template arguments removed; None for a built-in.
pub fn type_key(text: &str) -> Option<String> {
    // `struct tag { int x; }` is the class `tag`: key what precedes the body.
    // `struct { int x; }` leaves only `struct`, which names no class.
    let text = text.split('{').next().unwrap_or(text);
    let mut cleaned = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            '*' | '&' => cleaned.push(' '),
            _ => cleaned.push(c),
        }
    }
    const SKIPPED: [&str; 7] = ["const", "volatile", "struct", "class", "enum", "typename", "static"];
    const BUILTIN: [&str; 16] = [
        "int", "long", "short", "char", "float", "double", "bool", "void", "unsigned", "signed",
        "size_t", "auto", "wchar_t", "int8_t", "int16_t", "int32_t",
    ];
    cleaned
        .split_whitespace()
        .find(|word| !SKIPPED.contains(word) && !BUILTIN.contains(word))
        .map(str::to_string)
}

/// The parameter types of a C++ qualified name: `n::f(int,const char*)`.
fn params_of(qualified_name: &str) -> Vec<String> {
    let Some(open) = qualified_name.find('(') else {
        return Vec::new();
    };
    let Some(close) = qualified_name.rfind(')') else {
        return Vec::new();
    };
    let inside = &qualified_name[open + 1..close];
    let mut params = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for c in inside.chars() {
        match c {
            '<' | '(' | '[' => {
                depth += 1;
                current.push(c);
            }
            '>' | ')' | ']' => {
                depth = depth.saturating_sub(1);
                current.push(c);
            }
            ',' if depth == 0 => params.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        params.push(current);
    }
    params
}

/// How well an argument of the kind a call spells fits a parameter type.
fn fit(parameter: &str, argument: &str) -> i32 {
    let bare: String = parameter
        .replace("const", "")
        .replace(['&', ' '], "")
        .trim_matches('*')
        .to_string();
    let pointer = parameter.contains('*');
    let integral = ["int", "long", "short", "unsigned", "size_t", "char", "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t"];
    let floating = ["float", "double"];
    let is_integral = integral.iter().any(|t| bare.contains(t)) && !pointer;
    let is_floating = floating.iter().any(|t| bare == *t) && !pointer;
    // `&&` or `&&Class`: a temporary. It prefers `T&&`, then `const T&` or a copy, and
    // cannot bind a non-const `T&`.
    if let Some(class) = argument.strip_prefix("&&") {
        let matches = class.is_empty() || bare == class || bare.ends_with(&format!("::{class}"));
        if !matches {
            return 0;
        }
        return if parameter.contains("&&") {
            4
        } else if parameter.contains('&') && !parameter.contains("const") {
            -2
        } else {
            3
        };
    }
    match argument {
        "int" => {
            if is_integral && !bare.contains("char") { 3 } else if is_floating || bare == "bool" { 1 } else if pointer { 0 } else { -2 }
        }
        "float" => {
            if is_floating { 3 } else if is_integral { 1 } else { -2 }
        }
        "string" => {
            if pointer && bare.contains("char") || bare.contains("string") { 3 } else { -2 }
        }
        "char" => {
            if bare.contains("char") && !pointer { 3 } else if is_integral { 1 } else { -2 }
        }
        "bool" => {
            if bare == "bool" { 3 } else if is_integral { 1 } else { -2 }
        }
        "null" => {
            if pointer { 3 } else { -1 }
        }
        "?" => 0,
        named => {
            if bare == named || bare.ends_with(&format!("::{named}")) {
                // An lvalue prefers the copy over `T&&`.
                if parameter.contains("&&") { 1 } else { 3 }
            } else {
                0
            }
        }
    }
}

/// The candidate a call's arguments pick, and whether more than one fitted as well.
///
/// With no overloads this is the only one. With several, those whose parameter
/// count can take the call's arguments are kept and scored by how each argument's
/// spelling fits; a tie is a tie, and the first wins ambiguously.
fn pick<T: Clone>(candidates: &[T], qualified: impl Fn(&T) -> String, site: &CallSite) -> (T, bool) {
    if candidates.len() == 1 {
        return (candidates[0].clone(), false);
    }
    let mut scored: Vec<(i32, usize)> = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let params = params_of(&qualified(candidate));
        let score = match site.arity {
            Some(arity) if params.len() == arity => {
                1000 + params.iter().zip(&site.arg_kinds).map(|(p, a)| fit(p, a)).sum::<i32>()
            }
            // Defaults and variadics: close enough to try, worse than an exact count.
            Some(arity) if params.len() > arity => 500,
            Some(_) => -1000,
            None => 0,
        };
        scored.push((score, index));
    }
    let best = scored.iter().map(|(score, _)| *score).max().unwrap_or(0);
    let top: Vec<usize> = scored.iter().filter(|(score, _)| *score == best).map(|(_, i)| *i).collect();
    (candidates[top[0]].clone(), top.len() > 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(arity: usize, kinds: &[&str]) -> CallSite {
        CallSite {
            arity: Some(arity),
            arg_kinds: kinds.iter().map(|k| k.to_string()).collect(),
            ..CallSite::default()
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parameters_are_split_at_the_top_level_only() {
        assert_eq!(params_of("n::f(int,const char*)"), names(&["int", "const char*"]));
        assert_eq!(params_of("f(std::map<int,int>,int)"), names(&["std::map<int,int>", "int"]));
        assert_eq!(params_of("f(void(*)(int),int)"), names(&["void(*)(int)", "int"]));
        assert!(params_of("f()").is_empty());
        assert!(params_of("plain").is_empty());
    }

    #[test]
    fn a_type_reduces_to_the_class_it_names() {
        assert_eq!(type_key("const Shape&").as_deref(), Some("Shape"));
        assert_eq!(type_key("std::vector<Shape>*").as_deref(), Some("std::vector"));
        assert_eq!(type_key("struct engine *").as_deref(), Some("engine"));
        assert_eq!(type_key("unsigned long"), None);
        assert_eq!(type_key("const char*"), None);
    }

    #[test]
    fn an_overload_is_picked_by_how_the_arguments_are_spelled() {
        let candidates = names(&["f(int)", "f(double)", "f(const char*)"]);
        let pick_for = |s: &CallSite| pick(&candidates, |c| c.clone(), s);
        assert_eq!(pick_for(&site(1, &["int"])), ("f(int)".to_string(), false));
        assert_eq!(pick_for(&site(1, &["float"])), ("f(double)".to_string(), false));
        assert_eq!(pick_for(&site(1, &["string"])), ("f(const char*)".to_string(), false));
    }

    #[test]
    fn a_tie_picks_the_first_and_says_it_was_a_tie() {
        let candidates = names(&["f(A&)", "f(B&)"]);
        let (first, ambiguous) = pick(&candidates, |c| c.clone(), &site(1, &["?"]));
        assert_eq!(first, "f(A&)");
        assert!(ambiguous);
    }

    #[test]
    fn the_parameter_count_comes_before_the_types() {
        let candidates = names(&["f(int)", "f(int,int)"]);
        assert_eq!(pick(&candidates, |c| c.clone(), &site(2, &["int", "int"])), ("f(int,int)".to_string(), false));
    }

    #[test]
    fn a_member_is_looked_up_in_the_receivers_class_and_then_its_bases() {
        let mut index = CFamilyIndex::new();
        index.declare_class("Shape");
        index.declare_class("Sq");
        index.add_base("Sq", "Shape");
        index.declare_member("Shape", "area", "Shape::area()", NodeKind::Method);
        index.declare_member("Shape", "id", "Shape::id()", NodeKind::Method);
        index.declare_member("Sq", "area", "Sq::area()", NodeKind::Method);

        let receiver = |class: &str| CallSite { receiver_type: Some(class.to_string()), ..CallSite::default() };
        let found = |name: &str, class: &str| match index.resolve(name, "call", &receiver(class), "main.cpp") {
            Outcome::Found { qualified_name, .. } => Some(qualified_name),
            _ => None,
        };
        // The nearest class that has the name hides the others.
        assert_eq!(found("area", "Sq").as_deref(), Some("Sq::area()"));
        assert_eq!(found("area", "Shape").as_deref(), Some("Shape::area()"));
        // Inherited.
        assert_eq!(found("id", "Sq").as_deref(), Some("Shape::id()"));
        // A receiver of a type the project does not declare is not a project call.
        assert!(matches!(
            index.resolve("size", "call", &receiver("std::string"), "main.cpp"),
            Outcome::Outside
        ));
    }

    #[test]
    fn resolve_type_ignores_functions_and_variables() {
        let mut index = CFamilyIndex::new();
        index.declare("S", "S", NodeKind::Function, "a.h", false);
        index.declare("S", "S", NodeKind::Variable, "a.h", false);
        assert!(index.resolve_type("S", &[], false, "a.h").is_none());
    }

    #[test]
    fn an_unadorned_name_prefers_the_typedef_and_a_tag_prefers_the_class() {
        let mut index = CFamilyIndex::new();
        index.declare("S", "S", NodeKind::Class, "a.h", false);
        index.declare("S", "S", NodeKind::TypeAlias, "a.h", false);
        let (_, kind, ambiguous) = index.resolve_type("S", &[], false, "a.h").unwrap();
        assert!(matches!(kind, NodeKind::TypeAlias));
        assert!(!ambiguous);
        let (_, kind, ambiguous) = index.resolve_type("S", &[], true, "a.h").unwrap();
        assert!(matches!(kind, NodeKind::Class));
        assert!(!ambiguous);
    }

    #[test]
    fn a_written_scope_filters_candidates_without_a_bare_name_fallback() {
        let mut index = CFamilyIndex::new();
        index.declare("string", "mine::string", NodeKind::Class, "a.h", false);
        let std = vec!["std".to_string()];
        let mine = vec!["mine".to_string()];
        assert!(index.resolve_type("string", &std, false, "a.h").is_none());
        assert!(index.resolve_type("string", &mine, false, "a.h").is_some());
        // No scope written: any visible type of the name.
        assert!(index.resolve_type("string", &[], false, "a.h").is_some());
    }

    #[test]
    fn a_type_must_be_reachable_by_include() {
        let mut index = CFamilyIndex::new();
        index.declare("S", "S", NodeKind::Class, "a.h", false);
        assert!(index.resolve_type("S", &[], false, "main.c").is_none());
        index.include("main.c", "a.h");
        index.close_includes();
        assert!(index.resolve_type("S", &[], false, "main.c").is_some());
    }

    fn found(outcome: Outcome) -> Option<(String, NodeKind, bool)> {
        match outcome {
            Outcome::Found { qualified_name, kind, ambiguous } => Some((qualified_name, kind, ambiguous)),
            _ => None,
        }
    }

    fn member(root: Option<&str>, path: &[&str]) -> CallSite {
        CallSite {
            member: true,
            receiver_type: root.map(str::to_string),
            receiver_path: path.iter().map(|s| s.to_string()).collect(),
            ..CallSite::default()
        }
    }

    fn list_index() -> CFamilyIndex {
        let mut index = CFamilyIndex::new();
        for class in ["other", "Node", "List"] {
            index.declare_class(class);
            index.declare("len", &format!("{class}::len"), NodeKind::Attribute, "a.h", false);
            index.declare_field(class, "len", &format!("{class}::len"), "int");
        }
        index.declare_field("Node", "next", "Node::next", "Node*");
        index.declare_field("List", "head", "List::head", "Node*");
        index
    }

    #[test]
    fn a_field_read_follows_the_receiver_through_a_chain() {
        let index = list_index();
        let read = |site: CallSite| found(index.resolve("len", "read", &site, "a.h"));
        assert_eq!(read(member(Some("Node"), &["next"])), Some(("Node::len".into(), NodeKind::Attribute, false)));
        assert_eq!(read(member(Some("List"), &["head"])), Some(("Node::len".into(), NodeKind::Attribute, false)));
        assert_eq!(read(member(Some("List"), &[])), Some(("List::len".into(), NodeKind::Attribute, false)));
    }

    #[test]
    fn a_typedef_receiver_leads_to_its_class() {
        let mut index = list_index();
        index.declare_alias("node_t", "struct Node *");
        index.declare_alias("node_tt", "node_t");
        let got = found(index.resolve("len", "read", &member(Some("node_tt"), &[]), "a.h"));
        assert_eq!(got.map(|g| g.0).as_deref(), Some("Node::len"));
    }

    #[test]
    fn a_global_receiver_takes_the_type_of_the_variable() {
        let mut index = list_index();
        index.declare_typed("server", "server", NodeKind::Variable, "a.h", false, type_key("struct List"));
        let site = CallSite { member: true, receiver_name: Some("server".into()), ..CallSite::default() };
        let got = found(index.resolve("len", "read", &site, "a.h"));
        assert_eq!(got.map(|g| g.0).as_deref(), Some("List::len"));
    }

    #[test]
    fn a_known_type_without_the_field_is_outside() {
        let index = list_index();
        assert!(matches!(index.resolve("bogus", "read", &member(Some("Node"), &[]), "a.h"), Outcome::Outside));
        assert!(matches!(index.resolve("len", "read", &member(Some("FILE"), &[]), "a.h"), Outcome::Outside));
    }

    #[test]
    fn an_unknown_receiver_falls_back_to_fields_only_and_says_ambiguous() {
        let index = list_index();
        let site = CallSite { member: true, ..CallSite::default() };
        let got = found(index.resolve("len", "read", &site, "a.h")).unwrap();
        assert_eq!(got.1, NodeKind::Attribute);
        assert!(got.2);
    }

    #[test]
    fn an_initialiser_binds_to_the_field_of_its_class() {
        let mut index = CFamilyIndex::new();
        index.declare_class("Edit");
        index.declare_class("Set");
        index.declare_field("Edit", "next_", "Edit::next_", "int");
        index.declare_field("Set", "next_", "Set::next_", "int");
        let site = CallSite {
            class_scope: Some("Set".into()),
            in_initializer_list: true,
            ..CallSite::default()
        };
        assert_eq!(found(index.resolve("next_", "write", &site, "a.h")).map(|g| g.0).as_deref(), Some("Set::next_"));
        assert!(matches!(index.resolve("Set", "write", &site, "a.h"), Outcome::NotACall));
    }

    #[test]
    fn an_unqualified_call_skips_a_field_of_the_same_name() {
        let mut index = CFamilyIndex::new();
        index.declare("mstime", "srv::mstime", NodeKind::Attribute, "a.c", false);
        index.declare("mstime", "mstime", NodeKind::Function, "a.c", false);
        let got = found(index.resolve("mstime", "call", &CallSite::default(), "a.c")).unwrap();
        assert_eq!((got.0.as_str(), got.1), ("mstime", NodeKind::Function));
    }

    #[test]
    fn lookup_kinds_filters_before_choosing_the_first() {
        let mut index = CFamilyIndex::new();
        index.declare("mstime", "srv::mstime", NodeKind::Attribute, "a.c", false);
        index.declare("mstime", "mstime", NodeKind::Function, "a.c", false);
        assert_eq!(index.lookup("mstime", "a.c").unwrap().1, NodeKind::Attribute);
        let got = index.lookup_kinds("mstime", "a.c", |k| k == NodeKind::Function).unwrap();
        assert_eq!((got.0.as_str(), got.1, got.2), ("mstime", NodeKind::Function, false));
    }

    #[test]
    fn an_anonymous_definition_names_no_class() {
        assert_eq!(type_key("struct { int x; }"), None);
    }

    #[test]
    fn an_inline_named_definition_names_its_tag() {
        assert_eq!(type_key("struct tag { int x; }").as_deref(), Some("tag"));
    }

    #[test]
    fn a_class_named_like_a_typedef_answers_a_member_read() {
        let mut index = CFamilyIndex::new();
        index.declare_class("buf_t");
        index.declare("len", "buf_t::len", NodeKind::Attribute, "a.h", false);
        index.declare_field("buf_t", "len", "buf_t::len", "int");
        let got = found(index.resolve("len", "read", &member(Some("buf_t"), &[]), "a.h"));
        assert_eq!(got, Some(("buf_t::len".into(), NodeKind::Attribute, false)));
    }

    fn op_site(left: Option<&str>, right: &str) -> CallSite {
        CallSite {
            overloaded_operator: true,
            speculative: true,
            arity: Some(1),
            arg_kinds: vec![right.to_string()],
            receiver_type: left.map(str::to_string),
            ..CallSite::default()
        }
    }

    fn operator_index() -> CFamilyIndex {
        let mut index = CFamilyIndex::new();
        for class in ["S", "B", "D", "A", "C"] {
            index.declare_class(class);
        }
        index.add_base("D", "B");
        index
    }

    #[test]
    fn an_operator_member_is_found_through_a_base_class() {
        let mut index = operator_index();
        index.declare_member("B", "operator==", "B::operator==(const B&) const", NodeKind::Method);
        let got = found(index.resolve("operator==", "call", &op_site(Some("D"), "D"), "a.h"));
        assert_eq!(got, Some(("B::operator==(const B&) const".into(), NodeKind::Method, false)));
    }

    #[test]
    fn a_free_operator_is_chosen_by_the_type_of_its_parameters() {
        let mut index = operator_index();
        index.declare("operator!=", "operator!=(const S&,const S&)", NodeKind::Function, "a.h", false);
        let got = found(index.resolve("operator!=", "call", &op_site(Some("S"), "S"), "a.h"));
        assert_eq!(got, Some(("operator!=(const S&,const S&)".into(), NodeKind::Function, false)));
    }

    #[test]
    fn a_free_operator_needs_an_operand_of_the_class_it_names() {
        let mut index = operator_index();
        index.declare("operator==", "operator==(const S&,const S&)", NodeKind::Function, "a.h", false);
        // `int == int`: no operand class at all.
        let outcome = index.resolve("operator==", "call", &op_site(None, "int"), "a.h");
        assert!(matches!(outcome, Outcome::NotACall));
    }

    #[test]
    fn assignment_never_falls_back_to_a_free_function() {
        let mut index = operator_index();
        index.declare("operator=", "operator=(S&,const S&)", NodeKind::Function, "a.h", false);
        let outcome = index.resolve("operator=", "call", &op_site(Some("S"), "S"), "a.h");
        assert!(matches!(outcome, Outcome::NotACall));
    }

    #[test]
    fn a_pointer_field_is_not_an_operand_but_an_object_field_is() {
        let mut index = operator_index();
        index.declare_class("H");
        index.declare_field("H", "p_", "H::p_", "S*");
        index.declare_field("H", "v_", "H::v_", "S");
        index.declare_object("H::v_");
        index.declare_member("S", "operator=", "S::operator=(const S&)", NodeKind::Method);
        let on = |field: &str| CallSite {
            overloaded_operator: true,
            speculative: true,
            arity: Some(1),
            arg_kinds: vec!["S".to_string()],
            class_scope: Some("H".to_string()),
            receiver_name: Some(field.to_string()),
            ..CallSite::default()
        };
        let object = found(index.resolve("operator=", "call", &on("v_"), "a.h"));
        assert_eq!(object, Some(("S::operator=(const S&)".into(), NodeKind::Method, false)));
        assert!(matches!(index.resolve("operator=", "call", &on("p_"), "a.h"), Outcome::NotACall));
    }

    #[test]
    fn a_temporary_picks_the_move_and_a_named_object_the_copy() {
        let mut index = operator_index();
        index.declare_member("S", "operator=", "S::operator=(const S&)", NodeKind::Method);
        index.declare_member("S", "operator=", "S::operator=(S&&)", NodeKind::Method);
        let pick_for = |right: &str| {
            found(index.resolve("operator=", "call", &op_site(Some("S"), right), "a.h")).map(|f| (f.0, f.2))
        };
        assert_eq!(pick_for("&&S"), Some(("S::operator=(S&&)".to_string(), false)));
        assert_eq!(pick_for("&&"), Some(("S::operator=(S&&)".to_string(), false)));
        assert_eq!(pick_for("S"), Some(("S::operator=(const S&)".to_string(), false)));
    }

    #[test]
    fn two_equally_fitting_free_operators_are_ambiguous() {
        let mut index = operator_index();
        index.declare("operator==", "operator==(const A&,const B&)", NodeKind::Function, "a.h", false);
        index.declare("operator==", "operator==(const A&,const C&)", NodeKind::Function, "a.h", false);
        let got = found(index.resolve("operator==", "call", &op_site(Some("A"), "?"), "a.h"));
        assert_eq!(got.map(|g| g.2), Some(true));
    }

    #[test]
    fn fit_scores_temporaries_and_lvalues_against_rvalue_references() {
        assert_eq!(fit("T&&", "&&T"), 4);
        assert_eq!(fit("const T&", "&&T"), 3);
        assert_eq!(fit("T", "&&T"), 3);
        assert_eq!(fit("T&", "&&T"), -2);
        assert_eq!(fit("T&&", "T"), 1);
        assert_eq!(fit("const T&", "T"), 3);
    }
}
