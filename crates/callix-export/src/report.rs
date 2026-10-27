//! Human-readable reports over a graph: text, Markdown or JSON.
//!
//! A report is a title and a list of sections, each a set of key/value pairs,
//! a table or a note. The three renderers share that shape, so a new report
//! only has to build it.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;

use serde_json::{Map, Value, json};

use crate::error::{Error, Result};
use crate::lsif::language_id;
use crate::model::{Graph, Node, NodeKind, RelationKind};
use crate::{Options, Warning};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportKind {
    Summary,
    Status,
    Dependencies,
    Boundaries,
    Calls,
}

impl ReportKind {
    pub const ALL: [Self; 5] =
        [Self::Summary, Self::Status, Self::Dependencies, Self::Boundaries, Self::Calls];

    pub fn name(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Status => "status",
            Self::Dependencies => "dependencies",
            Self::Boundaries => "boundaries",
            Self::Calls => "calls",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Summary => "node and relation counts, languages, largest files",
            Self::Status => "how complete symbol resolution was, and what that means per language",
            Self::Dependencies => "the packages the project declares",
            Self::Boundaries => "HTTP/gRPC/queue contracts: who exposes and who consumes them",
            Self::Calls => "callers and callees of a symbol (--symbol), or the busiest functions",
        }
    }

    pub fn parse(name: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.name() == name)
            .ok_or_else(|| Error::UnknownFormat(format!("report type {name:?}")))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Markdown,
    Json,
}

impl Format {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "text" => Ok(Self::Text),
            "md" | "markdown" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            other => Err(Error::UnknownFormat(format!("report format {other:?}"))),
        }
    }
}

pub enum Body {
    Pairs(Vec<(String, String)>),
    Table { headers: Vec<String>, rows: Vec<Vec<String>> },
    Note(String),
}

pub struct Section {
    pub heading: String,
    pub body: Body,
}

pub struct Report {
    pub title: String,
    pub sections: Vec<Section>,
}

fn pairs(heading: &str, items: Vec<(&str, String)>) -> Section {
    Section {
        heading: heading.to_owned(),
        body: Body::Pairs(items.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()),
    }
}

fn table(heading: &str, headers: &[&str], rows: Vec<Vec<String>>) -> Section {
    Section {
        heading: heading.to_owned(),
        body: Body::Table { headers: headers.iter().map(|h| (*h).to_owned()).collect(), rows },
    }
}

fn note(heading: &str, text: impl Into<String>) -> Section {
    Section { heading: heading.to_owned(), body: Body::Note(text.into()) }
}

/// Builds the report and returns it with the warnings the data deserves.
pub fn build(
    kind: ReportKind,
    graph: &Graph,
    options: &Options,
) -> (Report, Vec<Warning>) {
    let mut warnings = Vec::new();
    let report = match kind {
        ReportKind::Summary => summary(graph),
        ReportKind::Status => status(graph),
        ReportKind::Dependencies => dependencies(graph),
        ReportKind::Boundaries => boundaries(graph),
        ReportKind::Calls => calls(graph, options, &mut warnings),
    };
    (report, warnings)
}

fn language_counts(graph: &Graph) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for file in graph.nodes_of(NodeKind::File) {
        if let Some(path) = &file.file_path {
            *counts.entry(language_id(path)).or_insert(0) += 1;
        }
    }
    counts
}

fn summary(graph: &Graph) -> Report {
    let languages = language_counts(graph);
    let languages_line = if languages.is_empty() {
        "none".to_owned()
    } else {
        languages.iter().map(|(l, n)| format!("{l} ({n})")).collect::<Vec<_>>().join(", ")
    };
    let projects: Vec<&str> = graph.nodes_of(NodeKind::Project).map(|n| n.name.as_str()).collect();

    let node_rows = NodeKind::ALL
        .into_iter()
        .filter_map(|kind| {
            let n = graph.nodes_of(kind).count();
            (n > 0).then(|| vec![kind.as_str().to_owned(), n.to_string()])
        })
        .collect();
    let relation_rows = RelationKind::ALL
        .into_iter()
        .filter_map(|kind| {
            let n = graph.relations_of(kind).count();
            (n > 0).then(|| vec![kind.as_str().to_owned(), n.to_string()])
        })
        .collect();

    let mut per_file: HashMap<&str, usize> = HashMap::new();
    for node in &graph.nodes {
        if let (Some(path), false) = (&node.file_path, node.kind == NodeKind::File) {
            *per_file.entry(path.as_str()).or_insert(0) += 1;
        }
    }
    let mut largest: Vec<(&str, usize)> = per_file.into_iter().collect();
    largest.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let file_rows = largest
        .into_iter()
        .take(10)
        .map(|(path, n)| vec![path.to_owned(), n.to_string()])
        .collect();

    Report {
        title: "Graph summary".into(),
        sections: vec![
            pairs(
                "Overview",
                vec![
                    ("projects", if projects.is_empty() { "none".into() } else { projects.join(", ") }),
                    ("nodes", graph.nodes.len().to_string()),
                    ("relations", graph.relations.len().to_string()),
                    ("files", graph.nodes_of(NodeKind::File).count().to_string()),
                    ("languages", languages_line),
                    ("resolver status", graph.resolver_status().unwrap_or("unknown").to_owned()),
                ],
            ),
            table("Nodes by kind", &["kind", "count"], node_rows),
            table("Relations by kind", &["kind", "count"], relation_rows),
            table("Largest files (by declared nodes)", &["file", "nodes"], file_rows),
        ],
    }
}

/// What each language's resolver does, and what it does not, for the status
/// report. It says how the answers are found - by types or by names - and the
/// kinds of call that are known to be missed, because "ok" alone promises more
/// than a type checker without a points-to analysis can deliver.
fn language_resolution(language: &str) -> (&'static str, &'static str) {
    match language {
        "python" => (
            "ty (embedded), by types",
            "calls through a name or an attribute whose type is known; a function carried in a \
             container, an argument or a return value is followed by a flow pass and marked `flow`; \
             decorators, receivers, keyed containers and the library's own callbacks (`map`, \
             `sorted(key=)`) are followed by the same pass; the pass is flow- and context-insensitive; \
             `getattr` and `eval` are not followed",
        ),
        "typescript" | "javascript" => (
            "typescript-go (embedded), by types",
            "calls through a name or a member whose type is known; function values are followed by \
             a flow pass and marked `flow` (destructuring, literal keys, `bind`/`call`/`apply`, a \
             closed list of library callbacks); `any`, dynamic `import()` and computed keys are \
             not followed",
        ),
        "go" => (
            "go/packages + go/types, by types; needs the Go toolchain",
            "static calls and concrete methods; a function value is followed by a flow pass and \
             marked `flow` (slices, range loops, channels, variadic parameters, a closed list of \
             standard-library callbacks); a call through an interface lands on the interface's \
             method, never on an implementation; every named type in a parameter, result \
             (functions and interface methods) or field is a `has_type`, but predeclared types \
             and type parameters are not",
        ),
        "rust" => (
            "rust-analyzer scip, by types; needs rust-analyzer and Cargo",
            "static calls and concrete methods; a function value is followed by a flow pass and \
             marked `flow` (arrays and `for` loops, a closed list of iterator and thread callbacks); \
             the calls in macro arguments are found and a `macro_rules!` of the same file is \
             expanded; a call through a trait lands on the trait's method, not on an impl; implicit \
             calls (`Drop`, operators, `for` over an iterator) are not edges",
        ),
        "php" => (
            "symbol table, by names",
            "approximate: no type inference, so a method is found by the declared type of its \
             receiver (or a `new` assigned to it) when there is one and otherwise by name; an \
             undeclared method goes to `__call` when the class declares it; a callable named by \
             a string, an array or `g(...)` is followed to the parameter, local or property that \
             holds it by a flow-insensitive pass (`flow`), and inside the built-ins that call it",
        ),
        "c" | "cpp" => (
            "symbol table, by names",
            "approximate: a member call is bound by the receiver's declared type and the class's \
             bases, overloads by arity and the look of the arguments, `#include` reachability for \
             the rest; no type checking, so a receiver whose type is not declared falls back to \
             the name (such edges say `ambiguous`); macros of the project are expanded where they \
             are invoked, conditionals are not evaluated, and a callback handed to a library \
             function is followed only for a closed list of them",
        ),
        "yaml" => ("none", "declares no symbols"),
        _ => ("none", "not analysed"),
    }
}

/// How the edges of one kind were resolved, counted from the edges themselves.
fn resolution_rows(graph: &Graph, kind: RelationKind) -> Option<Vec<String>> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut ambiguous = 0usize;
    let mut total = 0usize;
    for relation in graph.relations_of(kind) {
        total += 1;
        let how = relation.metadata.get("resolution").and_then(Value::as_str).unwrap_or("unrecorded");
        *counts.entry(how).or_default() += 1;
        if relation.metadata.get("ambiguous").and_then(Value::as_bool).unwrap_or(false) {
            ambiguous += 1;
        }
    }
    if total == 0 {
        return None;
    }
    let part = |key: &str| counts.get(key).copied().unwrap_or(0);
    Some(vec![
        kind.as_str().to_uppercase(),
        total.to_string(),
        part("definition").to_string(),
        part("value").to_string(),
        part("external").to_string(),
        part("unlocated").to_string(),
        part("name").to_string(),
        ambiguous.to_string(),
    ])
}

fn status(graph: &Graph) -> Report {
    let metrics = graph.metadata.get("resolver_metrics").and_then(Value::as_object);
    let metric = |key: &str| -> Option<f64> { metrics?.get(key)?.as_f64() };
    let status = graph.resolver_status().unwrap_or("unknown");
    let meaning = match status {
        "ok" => "every language in the graph was resolved by its backend",
        "degraded" => "at least one language was resolved by a symbol table, not a type checker; \
                       reference edges are approximate",
        "unavailable" => "no resolution ran (resolve=False, or a toolchain was missing); the \
                          graph is structural: no CALLS, REFERENCES, HAS_TYPE or INHERITS_FROM",
        _ => "the graph records no resolver status",
    };

    let mut items = vec![("status", status.to_owned()), ("meaning", meaning.to_owned())];
    if let Some(queries) = metric("queries") {
        let resolved = metric("resolved").unwrap_or(0.0);
        let pct = metric("resolved_pct").unwrap_or(if queries > 0.0 { resolved * 100.0 / queries } else { 0.0 });
        items.push(("queries", format!("{queries:.0}")));
        items.push(("resolved", format!("{resolved:.0} ({pct:.1}%)")));
        for key in ["internal", "external", "unresolved", "to_value", "unlocated"] {
            if let Some(v) = metric(key) {
                items.push((key, format!("{v:.0}")));
            }
        }
        if let Some(seconds) = metric("seconds") {
            items.push(("resolver seconds", format!("{seconds:.1}")));
        }
    }

    let language_rows = language_counts(graph)
        .into_iter()
        .map(|(language, files)| {
            let (backend, quality) = language_resolution(language);
            vec![language.to_owned(), files.to_string(), backend.to_owned(), quality.to_owned()]
        })
        .collect();

    let edge_rows: Vec<Vec<String>> = [
        RelationKind::Calls,
        RelationKind::References,
        RelationKind::HasType,
        RelationKind::InheritsFrom,
    ]
    .into_iter()
    .filter_map(|kind| resolution_rows(graph, kind))
    .collect();

    Report {
        title: "Resolver status".into(),
        sections: vec![
            pairs("Resolution", items),
            table(
                "How the edges were resolved",
                &["edges", "total", "definition", "value", "external", "unlocated", "name", "ambiguous"],
                edge_rows,
            ),
            table("Per language", &["language", "files", "backend", "known limits"], language_rows),
        ],
    }
}

fn dependencies(graph: &Graph) -> Report {
    let mut declared: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for rel in graph.relations_of(RelationKind::DependsOn) {
        let (Some(from), Some(to)) = (graph.node(&rel.source_id), graph.node(&rel.target_id))
        else {
            continue;
        };
        if to.kind == NodeKind::Dependency {
            declared.entry(to.name.as_str()).or_default().push(from.name.as_str());
        }
    }
    let rows = declared
        .into_iter()
        .map(|(name, mut by)| {
            by.sort_unstable();
            by.dedup();
            vec![name.to_owned(), by.join(", ")]
        })
        .collect::<Vec<_>>();
    let count = rows.len();
    Report {
        title: "Dependencies".into(),
        sections: vec![
            pairs("Overview", vec![("declared packages", count.to_string())]),
            table("Declared", &["package", "declared by"], rows),
        ],
    }
}

fn boundaries(graph: &Graph) -> Report {
    let mut sides: HashMap<&str, (Vec<&Node>, Vec<&Node>)> = HashMap::new();
    for (kind, server) in [(RelationKind::Exposes, true), (RelationKind::Consumes, false)] {
        for rel in graph.relations_of(kind) {
            let (Some(from), Some(to)) = (graph.node(&rel.source_id), graph.node(&rel.target_id))
            else {
                continue;
            };
            if to.kind != NodeKind::Boundary {
                continue;
            }
            let entry = sides.entry(to.id.as_str()).or_default();
            if server { entry.0.push(from) } else { entry.1.push(from) }
        }
    }
    let names = |nodes: &[&Node]| {
        let mut v: Vec<&str> = nodes.iter().map(|n| n.qualified_name.as_str()).collect();
        v.sort_unstable();
        v.dedup();
        v.join(", ")
    };
    let rows = graph
        .nodes_of(NodeKind::Boundary)
        .map(|b| {
            let (exposed, consumed) = sides.get(b.id.as_str()).cloned().unwrap_or_default();
            vec![
                b.meta_str("mechanism").unwrap_or("").to_owned(),
                b.meta_str("key").unwrap_or(&b.name).to_owned(),
                names(&exposed),
                names(&consumed),
            ]
        })
        .collect::<Vec<_>>();
    let links: Vec<Vec<String>> = graph
        .relations_of(RelationKind::CommunicatesWith)
        .filter_map(|rel| {
            let (a, b) = (graph.node(&rel.source_id)?, graph.node(&rel.target_id)?);
            Some(vec![a.qualified_name.clone(), b.qualified_name.clone()])
        })
        .collect();

    let mut sections = vec![
        pairs("Overview", vec![("boundaries", rows.len().to_string())]),
        table("Boundaries", &["mechanism", "key", "exposed by", "consumed by"], rows),
    ];
    if !links.is_empty() {
        sections.push(table("Service links", &["from", "to"], links));
    }
    Report { title: "Boundaries".into(), sections }
}

fn calls(graph: &Graph, options: &Options, warnings: &mut Vec<Warning>) -> Report {
    let call_count = graph.relations_of(RelationKind::Calls).count();
    if call_count == 0 {
        warnings.push(Warning::new(
            "no-calls",
            "the graph has no CALLS edges (resolution off or unavailable)",
        ));
    }

    if let Some(symbol) = &options.symbol {
        let targets: Vec<&Node> = graph
            .nodes
            .iter()
            .filter(|n| n.id == *symbol || n.qualified_name == *symbol)
            .collect();
        if targets.is_empty() {
            warnings.push(Warning::new("no-symbol", format!("no node matches {symbol:?}")));
        }
        let mut sections = Vec::new();
        for node in targets {
            let name = |id: &str| {
                graph
                    .node(id)
                    .map_or_else(|| id.to_owned(), |n| format!("{} ({})", n.qualified_name, n.kind.as_str()))
            };
            let callers: Vec<Vec<String>> = graph
                .relations_of(RelationKind::Calls)
                .filter(|r| r.target_id == node.id)
                .map(|r| vec![name(&r.source_id), r.span().map_or(String::new(), |s| format!("{}:{}", s.start_line, s.start_col))])
                .collect();
            let callees: Vec<Vec<String>> = graph
                .relations_of(RelationKind::Calls)
                .filter(|r| r.source_id == node.id)
                .map(|r| vec![name(&r.target_id), r.span().map_or(String::new(), |s| format!("{}:{}", s.start_line, s.start_col))])
                .collect();
            sections.push(note(
                &node.qualified_name,
                format!("{} in {}", node.kind.as_str(), node.file_path.as_deref().unwrap_or("-")),
            ));
            sections.push(table("Callers", &["caller", "at line:col"], callers));
            sections.push(table("Callees", &["callee", "at line:col"], callees));
        }
        return Report { title: format!("Calls of {symbol}"), sections };
    }

    let mut incoming: HashMap<&str, usize> = HashMap::new();
    let mut outgoing: HashMap<&str, usize> = HashMap::new();
    for rel in graph.relations_of(RelationKind::Calls) {
        *incoming.entry(rel.target_id.as_str()).or_insert(0) += 1;
        *outgoing.entry(rel.source_id.as_str()).or_insert(0) += 1;
    }
    let top = |counts: HashMap<&str, usize>| -> Vec<Vec<String>> {
        let mut v: Vec<(&str, usize)> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        v.into_iter()
            .take(20)
            .map(|(id, n)| {
                let node = graph.node(id);
                vec![
                    node.map_or_else(|| id.to_owned(), |n| n.qualified_name.clone()),
                    node.and_then(|n| n.file_path.clone()).unwrap_or_default(),
                    n.to_string(),
                ]
            })
            .collect()
    };
    Report {
        title: "Calls".into(),
        sections: vec![
            pairs("Overview", vec![("call edges", call_count.to_string())]),
            table("Most called", &["symbol", "file", "calls in"], top(incoming)),
            table("Most calling", &["symbol", "file", "calls out"], top(outgoing)),
        ],
    }
}

impl Report {
    pub fn render(&self, format: Format, out: &mut dyn Write) -> Result<()> {
        match format {
            Format::Json => {
                serde_json::to_writer_pretty(&mut *out, &self.to_json())?;
                out.write_all(b"\n")?;
            }
            Format::Markdown => self.render_markdown(out)?,
            Format::Text => self.render_text(out)?,
        }
        Ok(())
    }

    fn to_json(&self) -> Value {
        let sections: Vec<Value> = self
            .sections
            .iter()
            .map(|s| match &s.body {
                Body::Pairs(items) => {
                    let map: Map<String, Value> =
                        items.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
                    json!({"heading": s.heading, "values": map})
                }
                Body::Table { headers, rows } => {
                    let rows: Vec<Value> = rows
                        .iter()
                        .map(|row| {
                            let map: Map<String, Value> = headers
                                .iter()
                                .zip(row)
                                .map(|(h, v)| (h.clone(), json!(v)))
                                .collect();
                            Value::Object(map)
                        })
                        .collect();
                    json!({"heading": s.heading, "rows": rows})
                }
                Body::Note(text) => json!({"heading": s.heading, "note": text}),
            })
            .collect();
        json!({"title": self.title, "sections": sections})
    }

    fn render_markdown(&self, out: &mut dyn Write) -> Result<()> {
        writeln!(out, "# {}\n", self.title)?;
        for section in &self.sections {
            writeln!(out, "## {}\n", section.heading)?;
            match &section.body {
                Body::Pairs(items) => {
                    for (k, v) in items {
                        writeln!(out, "- **{k}**: {v}")?;
                    }
                }
                Body::Table { headers, rows } => {
                    if rows.is_empty() {
                        writeln!(out, "_none_")?;
                    } else {
                        writeln!(out, "| {} |", headers.join(" | "))?;
                        writeln!(out, "|{}|", vec!["---"; headers.len()].join("|"))?;
                        for row in rows {
                            let cells: Vec<String> =
                                row.iter().map(|c| c.replace('|', "\\|")).collect();
                            writeln!(out, "| {} |", cells.join(" | "))?;
                        }
                    }
                }
                Body::Note(text) => writeln!(out, "{text}")?,
            }
            writeln!(out)?;
        }
        Ok(())
    }

    fn render_text(&self, out: &mut dyn Write) -> Result<()> {
        writeln!(out, "{}", self.title)?;
        writeln!(out, "{}", "=".repeat(self.title.chars().count()))?;
        for section in &self.sections {
            writeln!(out, "\n{}", section.heading)?;
            writeln!(out, "{}", "-".repeat(section.heading.chars().count()))?;
            match &section.body {
                Body::Pairs(items) => {
                    let width = items.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
                    for (k, v) in items {
                        writeln!(out, "{k:<width$}  {v}")?;
                    }
                }
                Body::Table { headers, rows } => {
                    if rows.is_empty() {
                        writeln!(out, "(none)")?;
                        continue;
                    }
                    let widths: Vec<usize> = (0..headers.len())
                        .map(|i| {
                            rows.iter()
                                .map(|r| r[i].chars().count())
                                .chain([headers[i].chars().count()])
                                .max()
                                .unwrap_or(0)
                        })
                        .collect();
                    let line = |cells: &[String]| {
                        cells
                            .iter()
                            .zip(&widths)
                            .map(|(c, w)| format!("{c:<w$}"))
                            .collect::<Vec<_>>()
                            .join("  ")
                            .trim_end()
                            .to_owned()
                    };
                    writeln!(out, "{}", line(headers))?;
                    for row in rows {
                        writeln!(out, "{}", line(row))?;
                    }
                }
                Body::Note(text) => writeln!(out, "{text}")?,
            }
        }
        Ok(())
    }
}
