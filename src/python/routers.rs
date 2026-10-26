//! URL prefixes that a router gets from another module.
//!
//! In a FastAPI or Flask project the router is usually built in one module,
//! decorated with routes there, and mounted in another:
//!
//! ```text
//! # routers/users.py
//! router = APIRouter(prefix="/users")
//! @router.get("/{id}")
//!
//! # main.py
//! from .routers import users
//! app.include_router(users.router, prefix="/api")
//! ```
//!
//! The route's public path is `/api/users/{id}`, and no single file says so.
//! Every file contributes its [`RouterFacts`] while the adapter walks the
//! project; [`apply`] then follows the imports from each route's router up
//! through the mounts and rewrites the key.
//!
//! Only what the sources state is followed: a router passed through a function,
//! built in a loop or reached through `getattr` keeps the prefix its own file
//! gave it.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node as TsNode;

use crate::boundaries::{BoundaryRef, join_http_path, normalize_http_path};
use crate::ts;

const PREFIX_KWARGS: [&str; 2] = ["prefix", "url_prefix"];
const MOUNT_METHODS: [&str; 2] = ["include_router", "register_blueprint"];
/// How many mounts deep a router may sit before the chain is cut. Real
/// projects use two or three levels; the cap keeps a cycle from looping.
const MAX_DEPTH: usize = 8;

/// What a name was bound to by an `import`.
#[derive(Debug, Clone)]
pub enum Import {
    /// `import a.b as c`: the name is the module `a.b`.
    Module(String),
    /// `from spec import name`: a module or a variable of `spec`, told apart
    /// once the project's modules are known. `spec` may start with dots.
    From(String, String),
}

/// `parent.include_router(child, prefix=...)`, with both sides as written.
#[derive(Debug, Clone)]
pub struct Mount {
    pub parent: String,
    pub child: String,
    pub prefix: String,
}

/// What one file says about routers.
#[derive(Debug, Default, Clone)]
pub struct RouterFacts {
    pub imports: HashMap<String, Import>,
    /// `router = APIRouter(prefix="/users")`: variable → prefix.
    pub own_prefix: HashMap<String, String>,
    pub mounts: Vec<Mount>,
}

/// A file's facts, with the module it is.
pub struct FileFacts {
    /// Project-relative path, as the boundaries are keyed.
    pub file: String,
    pub module: String,
    /// `__init__.py`: relative imports start from the module itself.
    pub is_package: bool,
    pub facts: RouterFacts,
}

fn text(node: TsNode<'_>, source: &[u8]) -> String {
    ts::text(node, source).into_owned()
}

/// A string literal's content, with an f-string interpolation as `{}`.
fn string_value(node: TsNode<'_>, source: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut parts = Vec::new();
    for child in ts::children(node) {
        match child.kind() {
            "string_content" => parts.push(text(child, source)),
            "interpolation" => parts.push("{}".to_string()),
            _ => {}
        }
    }
    Some(parts.concat())
}

/// The value of a keyword argument, as a string, when it is one.
fn kwarg_string(args: TsNode<'_>, wanted: &[&str], source: &[u8]) -> Option<String> {
    for child in ts::named_children(args) {
        if child.kind() != "keyword_argument" {
            continue;
        }
        let (Some(name), Some(value)) =
            (child.child_by_field_name("name"), child.child_by_field_name("value"))
        else {
            continue;
        };
        if wanted.contains(&text(name, source).as_str())
            && let Some(value) = string_value(value, source)
        {
            return Some(value);
        }
    }
    None
}

/// `a`, `a.b`, `a.b.c` — and nothing else, so a call or a subscript is not
/// mistaken for a name.
fn dotted(node: TsNode<'_>, source: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "identifier" | "attribute") {
        return None;
    }
    let name = text(node, source);
    name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.').then_some(name)
}

/// Reads the imports, router constructors and mounts of one file.
pub fn collect(root: TsNode<'_>, source: &[u8]) -> RouterFacts {
    let mut facts = RouterFacts::default();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(ts::children(node));
        match node.kind() {
            "import_statement" => import_statement(node, source, &mut facts),
            "import_from_statement" => import_from(node, source, &mut facts),
            "assignment" => {
                let (Some(left), Some(right)) =
                    (node.child_by_field_name("left"), node.child_by_field_name("right"))
                else {
                    continue;
                };
                if left.kind() != "identifier" || right.kind() != "call" {
                    continue;
                }
                let Some(args) = right.child_by_field_name("arguments") else { continue };
                if let Some(prefix) = kwarg_string(args, &PREFIX_KWARGS, source) {
                    facts.own_prefix.insert(text(left, source), prefix);
                }
            }
            "call" => {
                let Some(function) = node.child_by_field_name("function") else { continue };
                if function.kind() != "attribute" {
                    continue;
                }
                let (Some(object), Some(method)) =
                    (function.child_by_field_name("object"), function.child_by_field_name("attribute"))
                else {
                    continue;
                };
                if !MOUNT_METHODS.contains(&text(method, source).as_str()) {
                    continue;
                }
                let Some(args) = node.child_by_field_name("arguments") else { continue };
                let child = ts::named_children(args)
                    .into_iter()
                    .next()
                    .filter(|n| n.kind() != "keyword_argument")
                    .and_then(|n| dotted(n, source));
                let (Some(parent), Some(child)) = (dotted(object, source), child) else { continue };
                facts.mounts.push(Mount {
                    parent,
                    child,
                    prefix: kwarg_string(args, &PREFIX_KWARGS, source).unwrap_or_default(),
                });
            }
            _ => {}
        }
    }
    facts
}

/// `import a.b.c` binds `a`; `import a.b.c as d` binds `d` to `a.b.c`.
fn import_statement(node: TsNode<'_>, source: &[u8], facts: &mut RouterFacts) {
    for child in ts::named_children(node) {
        match child.kind() {
            "dotted_name" => {
                let name = text(child, source);
                let first = name.split('.').next().unwrap_or("").to_string();
                facts.imports.insert(first.clone(), Import::Module(first));
            }
            "aliased_import" => {
                let (Some(name), Some(alias)) =
                    (child.child_by_field_name("name"), child.child_by_field_name("alias"))
                else {
                    continue;
                };
                facts.imports.insert(text(alias, source), Import::Module(text(name, source)));
            }
            _ => {}
        }
    }
}

/// `from spec import a, b as c`.
fn import_from(node: TsNode<'_>, source: &[u8], facts: &mut RouterFacts) {
    let Some(spec) = node.child_by_field_name("module_name") else { return };
    let spec = text(spec, source);
    let mut cursor = node.walk();
    for name in node.children_by_field_name("name", &mut cursor) {
        match name.kind() {
            "dotted_name" => {
                let imported = text(name, source);
                facts.imports.insert(imported.clone(), Import::From(spec.clone(), imported));
            }
            "aliased_import" => {
                let (Some(original), Some(alias)) =
                    (name.child_by_field_name("name"), name.child_by_field_name("alias"))
                else {
                    continue;
                };
                facts.imports.insert(text(alias, source), Import::From(spec.clone(), text(original, source)));
            }
            _ => {}
        }
    }
}

/// A router is a variable of a module.
type RouterId = (String, String);

struct World<'a> {
    /// Files in module order, so the first of several mounts is stable.
    ordered: Vec<&'a FileFacts>,
    by_module: HashMap<&'a str, &'a FileFacts>,
    modules: HashSet<&'a str>,
}

impl<'a> World<'a> {
    fn new(files: &'a [FileFacts]) -> Self {
        let mut ordered: Vec<&FileFacts> = files.iter().collect();
        ordered.sort_by(|a, b| a.module.cmp(&b.module).then_with(|| a.file.cmp(&b.file)));
        Self {
            by_module: ordered.iter().map(|f| (f.module.as_str(), *f)).collect(),
            modules: files.iter().map(|f| f.module.as_str()).collect(),
            ordered,
        }
    }

    /// `.routers` seen from `app.main` is `app.routers`.
    fn resolve_spec(spec: &str, from: &FileFacts) -> Option<String> {
        let dots = spec.chars().take_while(|c| *c == '.').count();
        let rest = &spec[dots..];
        if dots == 0 {
            return Some(rest.to_string());
        }
        let mut base: Vec<&str> = from.module.split('.').collect();
        if !from.is_package {
            base.pop();
        }
        for _ in 1..dots {
            base.pop()?;
        }
        let mut out = base.join(".");
        if !rest.is_empty() {
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(rest);
        }
        Some(out)
    }

    /// The router an expression written in `file` names: `router`, an imported
    /// `router`, or `users.router` through an imported module.
    fn resolve(&self, file: &FileFacts, expr: &str) -> Option<RouterId> {
        let segments: Vec<&str> = expr.split('.').collect();
        let first = *segments.first()?;
        let (mut module, mut index) = match file.facts.imports.get(first) {
            Some(Import::Module(module)) => (module.clone(), 1),
            Some(Import::From(spec, name)) => {
                let base = Self::resolve_spec(spec, file)?;
                let full = if base.is_empty() { name.clone() } else { format!("{base}.{name}") };
                if self.modules.contains(full.as_str()) {
                    (full, 1)
                } else if segments.len() == 1 {
                    // `from .users import router`: a variable of that module.
                    return Some((base, name.clone()));
                } else {
                    return None;
                }
            }
            None if segments.len() == 1 => return Some((file.module.clone(), first.to_string())),
            None => return None,
        };
        while index + 1 < segments.len() {
            let next = format!("{module}.{}", segments[index]);
            if !self.modules.contains(next.as_str()) {
                return None;
            }
            module = next;
            index += 1;
        }
        segments.get(index).map(|name| (module, (*name).to_string()))
    }

    /// The prefix in front of a router's own routes: what it is mounted under,
    /// then its own.
    fn prefix_of(&self, id: &RouterId, depth: usize, memo: &mut HashMap<RouterId, String>) -> String {
        if let Some(known) = memo.get(id) {
            return known.clone();
        }
        let own = self
            .by_module
            .get(id.0.as_str())
            .and_then(|file| file.facts.own_prefix.get(&id.1))
            .cloned()
            .unwrap_or_default();
        let mut full = own.clone();
        if depth < MAX_DEPTH {
            'mounts: for file in &self.ordered {
                for mount in &file.facts.mounts {
                    if self.resolve(file, &mount.child).as_ref() != Some(id) {
                        continue;
                    }
                    let above = self
                        .resolve(file, &mount.parent)
                        .map(|parent| self.prefix_of(&parent, depth + 1, memo))
                        .unwrap_or_default();
                    full = join_http_path(&join_http_path(&above, &mount.prefix), &own);
                    break 'mounts;
                }
            }
        }
        memo.insert(id.clone(), full.clone());
        full
    }
}

/// Rewrites the key of every HTTP route whose router can be followed through
/// the project. The extractor leaves `router` and `route` in the detail for
/// this; they are removed here, resolved or not.
pub fn apply(files: &[FileFacts], boundaries: &mut [(String, String, Vec<BoundaryRef>)]) {
    let world = World::new(files);
    let by_file: HashMap<&str, &FileFacts> = files.iter().map(|f| (f.file.as_str(), f)).collect();
    let mut memo: HashMap<RouterId, String> = HashMap::new();

    for (file_rel, _, refs) in boundaries.iter_mut() {
        let file = by_file.get(file_rel.as_str()).copied();
        for reference in refs.iter_mut() {
            if reference.mechanism != "http" || reference.role != "server" {
                continue;
            }
            let router = reference.detail.shift_remove("router");
            let route = reference.detail.shift_remove("route");
            let (Some(file), Some(router), Some(route)) = (file, router, route) else { continue };
            let Some(verb) = reference.detail.get("method").cloned() else { continue };
            let Some(id) = world.resolve(file, &router) else { continue };

            let prefix = world.prefix_of(&id, 0, &mut memo);
            let full = normalize_http_path(&join_http_path(&prefix, &route));
            reference.key = format!("{verb} {full}");
            reference.detail.insert("path".to_string(), full);
        }
    }
}
