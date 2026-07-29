//! Cross-language boundaries declared in YAML.
//!
//! YAML holds no functions to call, which is exactly why it matters here: an
//! OpenAPI document states the routes a service serves without any code saying
//! so, and a Kubernetes Ingress states the paths that reach it. Because a
//! boundary's ID comes from the mechanism and the normalized key alone, a
//! route written in a spec lands on the same node as the handler that
//! implements it and the client that calls it — which is what makes "does the
//! spec match the code" answerable.

use indexmap::IndexMap;
use tree_sitter::Node as TsNode;

use crate::boundaries::{BoundaryRef, normalize_http_path};
use crate::ts;

const HTTP_VERBS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

/// What a document turned out to be.
///
/// YAML has no manifest to consult, so the flavour is read off the top-level
/// keys and the file name — the way a person recognizes the file at a glance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavour {
    /// `openapi:` or `swagger:` at the top level.
    OpenApi,
    /// `services:` at the top level — Compose.
    Compose,
    /// `apiVersion:` and `kind:` — a Kubernetes manifest.
    Kubernetes,
    /// A GitLab CI pipeline.
    GitLabCi,
    /// A GitHub Actions workflow.
    GitHubActions,
    /// `resources:` — a kustomization.
    Kustomize,
    /// A Helm `Chart.yaml`.
    HelmChart,
    /// A Go-template document: valid YAML only once rendered.
    HelmTemplate,
    Unknown,
}

impl Flavour {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenApi => "openapi",
            Self::Compose => "compose",
            Self::Kubernetes => "kubernetes",
            Self::GitLabCi => "gitlab-ci",
            Self::GitHubActions => "github-actions",
            Self::Kustomize => "kustomize",
            Self::HelmChart => "helm-chart",
            Self::HelmTemplate => "helm-template",
            Self::Unknown => "unknown",
        }
    }
}

/// The text of a scalar, unquoted.
fn scalar(node: TsNode<'_>, source: &[u8]) -> String {
    ts::text(node, source)
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string()
}

/// Every `key: value` pair of a mapping, in document order.
///
/// A YAML value arrives wrapped in `block_node` or `flow_node`; the wrapper is
/// unhelpful to callers, so it is peeled here once.
pub fn pairs<'tree>(node: TsNode<'tree>, source: &[u8]) -> Vec<(String, TsNode<'tree>)> {
    let mut out = Vec::new();
    let Some(mapping) = unwrap_node(node).filter(|n| n.kind() == "block_mapping") else {
        return out;
    };
    for pair in ts::children(mapping) {
        if pair.kind() != "block_mapping_pair" {
            continue;
        }
        let (Some(key), Some(value)) = (
            pair.child_by_field_name("key"),
            pair.child_by_field_name("value"),
        ) else {
            continue;
        };
        out.push((scalar(key, source), value));
    }
    out
}

/// Peels `block_node` / `flow_node` wrappers down to the content.
fn unwrap_node<'tree>(node: TsNode<'tree>) -> Option<TsNode<'tree>> {
    let mut current = node;
    loop {
        match current.kind() {
            "block_node" | "flow_node" | "document" | "stream" => {
                current = ts::named_children(current).into_iter().next()?;
            }
            _ => return Some(current),
        }
    }
}

/// Items of a sequence, block or flow.
///
/// Both spellings appear constantly in the same file — `needs: [build]` beside
/// a dashed list — so callers should not have to care which they got.
pub fn items<'tree>(node: TsNode<'tree>) -> Vec<TsNode<'tree>> {
    let Some(sequence) = unwrap_node(node) else {
        return Vec::new();
    };
    match sequence.kind() {
        "block_sequence" => ts::children(sequence)
            .into_iter()
            .filter(|c| c.kind() == "block_sequence_item")
            .filter_map(|item| ts::named_children(item).into_iter().next())
            .collect(),
        "flow_sequence" => ts::named_children(sequence),
        _ => Vec::new(),
    }
}

/// The document's root mapping, past the stream and document wrappers.
pub fn root_mapping<'tree>(root: TsNode<'tree>, source: &[u8]) -> Vec<(String, TsNode<'tree>)> {
    for document in ts::named_children(root) {
        let found = pairs(document, source);
        if !found.is_empty() {
            return found;
        }
    }
    pairs(root, source)
}

/// Recognizes the document from its top-level keys and its path.
///
/// The path matters because content alone is ambiguous: a `.gitlab-ci.yml` may
/// carry a top-level `services:` as a pipeline-wide default, which reads
/// exactly like Compose. CI is therefore checked first, and by name where the
/// name is decisive.
pub fn flavour(root: TsNode<'_>, source: &[u8], file_path: &str) -> Flavour {
    let keys: Vec<String> = root_mapping(root, source)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let has = |name: &str| keys.iter().any(|k| k == name);

    let name = file_path.rsplit('/').next().unwrap_or(file_path);
    let in_workflows = file_path.contains(".github/workflows/");

    // A Go template is not YAML yet. tree-sitter parses it anyway and produces
    // a mangled tree, so saying so outright is better than letting `path: {{
    // .Values.ingress.path }}` become a boundary key.
    if source.windows(2).any(|w| w == b"{{") && source.windows(2).any(|w| w == b"}}") {
        return Flavour::HelmTemplate;
    }
    if name == "Chart.yaml" || name == "Chart.yml" {
        return Flavour::HelmChart;
    }

    if name == ".gitlab-ci.yml" || name == ".gitlab-ci.yaml" {
        return Flavour::GitLabCi;
    }
    if in_workflows && has("jobs") {
        return Flavour::GitHubActions;
    }
    if has("openapi") || has("swagger") {
        Flavour::OpenApi
    } else if has("apiVersion") && has("kind") {
        Flavour::Kubernetes
    } else if has("resources") && name.starts_with("kustomization") {
        Flavour::Kustomize
    } else if has("services") {
        Flavour::Compose
    } else if has("jobs") && has("on") {
        // A workflow kept outside .github/workflows, e.g. a reusable one.
        Flavour::GitHubActions
    } else if has("stages") && has("include") {
        Flavour::GitLabCi
    } else {
        Flavour::Unknown
    }
}

/// A path or remote reference this document pulls in.
pub struct Include {
    /// The raw reference, relative to the including file's directory.
    pub target: String,
    /// True when the reference cannot be a local file — a remote URL, or a
    /// GitLab template shipped by the server.
    pub external: bool,
    /// Where the reference is written, so the edge can point at it.
    pub line: u32,
}

fn include_from_mapping(node: TsNode<'_>, source: &[u8]) -> Option<Include> {
    let line = ts::span(node).start_line;
    for (key, value) in pairs(node, source) {
        let target = scalar(value, source);
        match key.as_str() {
            // Compose `include: [{path: ./a.yml}]`, GitLab `local:` / `file:`.
            "path" | "local" | "file" => {
                return Some(Include { target, external: false, line });
            }
            // Neither resolves to a file in this tree.
            "remote" | "template" | "component" => {
                return Some(Include { target, external: true, line });
            }
            _ => {}
        }
    }
    None
}

/// Every file this document pulls in, whatever the mechanism.
///
/// The point is that multi-file YAML stops being a coincidence: `compose.yaml`
/// and `compose.db.yaml` are otherwise two unrelated documents that happen to
/// sit in one directory, and the graph should say that one includes the other.
pub fn includes(root: TsNode<'_>, source: &[u8], shape: Flavour) -> Vec<Include> {
    let mut out = Vec::new();
    let key_name = match shape {
        Flavour::Kustomize => "resources",
        Flavour::Compose | Flavour::GitLabCi => "include",
        // A GitHub workflow reaches another through `uses:`, handled with the
        // jobs; OpenAPI through `$ref`, which is scanned separately.
        _ => return out,
    };

    for (key, value) in root_mapping(root, source) {
        if key != key_name {
            continue;
        }
        // A reference may be written three ways, and the shape has to come
        // from the node's kind: a single-item block sequence renders on one
        // line, so guessing from the text mistook `- path: x` for a scalar.
        let sequence = items(value);
        if !sequence.is_empty() {
            for item in sequence {
                out.push(one_include(item, source));
            }
        } else {
            out.push(one_include(value, source));
        }
    }
    out.retain(|i| !i.target.is_empty());
    out
}

/// One reference, whether written as a mapping or as a bare path.
fn one_include(node: TsNode<'_>, source: &[u8]) -> Include {
    if let Some(found) = include_from_mapping(node, source) {
        return found;
    }
    Include {
        target: scalar(node, source),
        external: false,
        line: ts::span(node).start_line,
    }
}

/// External `$ref` targets anywhere in the document.
///
/// OpenAPI splits itself across files with `$ref: './schemas.yaml#/User'`. A
/// fragment-only ref (`#/components/...`) points inside this document and is
/// not an include.
pub fn refs(root: TsNode<'_>, source: &[u8]) -> Vec<Include> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "block_mapping_pair"
            && let (Some(key), Some(value)) = (
                node.child_by_field_name("key"),
                node.child_by_field_name("value"),
            )
            && scalar(key, source) == "$ref"
        {
            let target = scalar(value, source);
            let path = target.split('#').next().unwrap_or_default().to_string();
            if !path.is_empty() {
                out.push(Include {
                    target: path,
                    external: target.starts_with("http"),
                    line: ts::span(value).start_line,
                });
            }
        }
        stack.extend(ts::children(node));
    }
    out
}

/// CI jobs and what each waits on.
///
/// A job is a unit of work that depends on other units — the same shape as a
/// Compose service, so it takes the same modelling.
pub struct Job {
    pub name: String,
    pub needs: Vec<String>,
    /// External actions a GitHub job uses (`owner/repo@ref`).
    pub uses_external: Vec<String>,
    /// Local workflows or actions it reaches (`./.github/workflows/x.yml`).
    pub uses_local: Vec<String>,
}

fn scalar_or_list(node: TsNode<'_>, source: &[u8]) -> Vec<String> {
    let listed: Vec<String> = items(node)
        .into_iter()
        .map(|n| scalar(n, source))
        .filter(|s| !s.is_empty())
        .collect();
    if !listed.is_empty() {
        return listed;
    }
    // A `needs:` entry may also be a mapping, as in `needs: [{job: build}]`.
    let mapped: Vec<String> = pairs(node, source)
        .into_iter()
        .filter(|(k, _)| k == "job")
        .map(|(_, v)| scalar(v, source))
        .collect();
    if !mapped.is_empty() {
        return mapped;
    }
    let single = scalar(node, source);
    if single.is_empty() || single.contains('\n') {
        Vec::new()
    } else {
        vec![single]
    }
}

/// Collects the jobs of a GitLab pipeline or a GitHub workflow.
pub fn jobs(root: TsNode<'_>, source: &[u8], shape: Flavour) -> Vec<Job> {
    let top = root_mapping(root, source);
    let entries = match shape {
        // GitHub nests them under `jobs:`.
        Flavour::GitHubActions => top
            .iter()
            .find(|(k, _)| k == "jobs")
            .map(|(_, v)| pairs(*v, source))
            .unwrap_or_default(),
        // GitLab puts them at the top level, mixed with configuration keys.
        Flavour::GitLabCi => top
            .iter()
            .filter(|(k, _)| !is_gitlab_keyword(k))
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
        _ => return Vec::new(),
    };

    entries
        .into_iter()
        .filter(|(name, _)| !name.is_empty() && !name.starts_with('.'))
        .map(|(name, body)| {
            let mut job = Job {
                name,
                needs: Vec::new(),
                uses_external: Vec::new(),
                uses_local: Vec::new(),
            };
            for (key, value) in pairs(body, source) {
                match key.as_str() {
                    "needs" | "dependencies" => job.needs.extend(scalar_or_list(value, source)),
                    "uses" => classify_uses(&scalar(value, source), &mut job),
                    // A GitHub job's steps each carry their own `uses:`.
                    "steps" => {
                        for step in items(value) {
                            for (step_key, step_value) in pairs(step, source) {
                                if step_key == "uses" {
                                    classify_uses(&scalar(step_value, source), &mut job);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            job
        })
        .collect()
}

fn classify_uses(target: &str, job: &mut Job) {
    if target.is_empty() {
        return;
    }
    if target.starts_with('.') || target.starts_with('/') {
        job.uses_local.push(target.to_string());
    } else {
        // `owner/repo@ref` — the version is not part of the identity.
        let name = target.split('@').next().unwrap_or(target);
        job.uses_external.push(name.to_string());
    }
}

/// Top-level GitLab keys that configure the pipeline rather than name a job.
fn is_gitlab_keyword(key: &str) -> bool {
    matches!(
        key,
        "stages"
            | "include"
            | "default"
            | "variables"
            | "workflow"
            | "image"
            | "services"
            | "before_script"
            | "after_script"
            | "cache"
            | "spec"
    )
}

fn http_ref(role: &str, verb: &str, path: &str, node: TsNode<'_>, source: &str) -> BoundaryRef {
    let norm = normalize_http_path(path);
    let span = ts::span(node);
    let mut detail = IndexMap::new();
    detail.insert("method".to_string(), verb.to_string());
    detail.insert("path".to_string(), norm.clone());
    detail.insert("source".to_string(), source.to_string());
    BoundaryRef {
        mechanism: "http".to_string(),
        role: role.to_string(),
        key: format!("{verb} {norm}"),
        line: span.start_line,
        col: span.start_col,
        // A specification states the contract outright; nothing is inferred.
        confidence: 1.0,
        detail,
    }
}

/// `paths:` of an OpenAPI document — every path crossed with every verb.
fn openapi(root: TsNode<'_>, source: &[u8]) -> Vec<BoundaryRef> {
    let mut refs = Vec::new();
    for (key, value) in root_mapping(root, source) {
        if key != "paths" {
            continue;
        }
        for (route, operations) in pairs(value, source) {
            for (verb, _) in pairs(operations, source) {
                if HTTP_VERBS.contains(&verb.to_lowercase().as_str()) {
                    refs.push(http_ref(
                        "server",
                        &verb.to_uppercase(),
                        &route,
                        value,
                        "openapi",
                    ));
                }
            }
        }
    }
    refs
}

/// `spec.rules[].http.paths[].path` of a Kubernetes Ingress.
///
/// An Ingress states which paths reach the cluster, and says nothing about the
/// method, so the key records it as ANY — the same shape `net/http`'s own
/// router produces.
fn kubernetes(root: TsNode<'_>, source: &[u8]) -> Vec<BoundaryRef> {
    let top = root_mapping(root, source);
    let kind = top
        .iter()
        .find(|(k, _)| k == "kind")
        .map(|(_, v)| scalar(*v, source))
        .unwrap_or_default();
    if kind != "Ingress" {
        return Vec::new();
    }

    let mut refs = Vec::new();
    for (key, spec) in &top {
        if key != "spec" {
            continue;
        }
        for (spec_key, rules) in pairs(*spec, source) {
            if spec_key != "rules" {
                continue;
            }
            for rule in items(rules) {
                for (rule_key, http) in pairs(rule, source) {
                    if rule_key != "http" {
                        continue;
                    }
                    for (http_key, paths) in pairs(http, source) {
                        if http_key != "paths" {
                            continue;
                        }
                        for entry in items(paths) {
                            for (entry_key, path) in pairs(entry, source) {
                                if entry_key == "path" {
                                    refs.push(http_ref(
                                        "server",
                                        "ANY",
                                        &scalar(path, source),
                                        path,
                                        "kubernetes-ingress",
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    refs
}

/// Every boundary a YAML document declares.
pub fn extract_boundaries(root: TsNode<'_>, source: &[u8], file_path: &str) -> Vec<BoundaryRef> {
    match flavour(root, source, file_path) {
        Flavour::OpenApi => openapi(root, source),
        Flavour::Kubernetes => kubernetes(root, source),
        // Compose services, CI jobs and includes become nodes and edges
        // rather than ports; see `adapter.rs`.
        _ => Vec::new(),
    }
}

/// Boundaries in a single source — the entry point for checks.
#[pyo3_stub_gen::derive::gen_stub_pyfunction]
#[pyo3::pyfunction]
pub fn extract_yaml_boundaries(
    source: &pyo3::Bound<'_, pyo3::types::PyBytes>,
) -> pyo3::PyResult<Vec<BoundaryRef>> {
    use pyo3::prelude::*;
    let source = source.as_bytes();
    let tree = super::parse_tree(source)?;
    Ok(extract_boundaries(tree.root_node(), source, ""))
}

/// Subcharts a `Chart.yaml` declares.
///
/// A chart's `dependencies:` are packages by any measure — named, versioned and
/// fetched from a repository — so they become DEPENDENCY nodes like the entries
/// of any other manifest.
pub fn chart_dependencies(root: TsNode<'_>, source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for (key, value) in root_mapping(root, source) {
        if key != "dependencies" {
            continue;
        }
        for item in items(value) {
            for (field, name) in pairs(item, source) {
                if field == "name" {
                    out.push(scalar(name, source));
                }
            }
        }
    }
    out.retain(|n| !n.is_empty());
    out
}
