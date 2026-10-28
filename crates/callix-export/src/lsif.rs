//! LSIF — the Language Server Index Format — as JSON Lines.
//!
//! The graph maps onto LSIF like this:
//!
//! | callix | LSIF |
//! |---|---|
//! | a file (`file_path` of any node) | `document` |
//! | a definition's `metadata["name_span"]` | `range` |
//! | a reference edge's `metadata["span"]` | `range` |
//! | a definition, or an external symbol | `resultSet` |
//! | what a symbol resolves to | `definitionResult` / `referenceResult` |
//! | kind and qualified name | `hoverResult` |
//! | `EXTERNAL_SYMBOL` | `moniker` (kind `import`) |
//!
//! Only edges that record where they were written carry a position, which is
//! the resolver's output. A graph built with `resolve=False` therefore exports
//! its definitions and nothing else, and says so in a warning.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::error::Result;
use crate::index::{Index, Pos, Symbol};
use crate::model::Graph;
use crate::{Exporter, Options, Warning};

/// The LSIF version claimed by default. 0.4.3 is what the widely deployed
/// indexers (lsif-go, lsif-node) write and what consumers such as GitLab
/// read; `positionEncoding` is a later addition they ignore.
pub use crate::index::language_id;

pub const DEFAULT_VERSION: &str = "0.4.3";

pub struct Lsif;

impl Exporter for Lsif {
    fn name(&self) -> &'static str {
        "lsif"
    }

    fn description(&self) -> &'static str {
        "Language Server Index Format (JSON Lines), for code navigation"
    }

    fn extension(&self) -> &'static str {
        "lsif"
    }

    fn export(
        &self,
        graph: &Graph,
        out: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Warning>> {
        let mut builder = Builder::new(graph, options);
        builder.emit(out)?;
        Ok(builder.warnings())
    }
}

/// `file://` + the path, with everything outside the URI-safe set escaped.
fn file_uri(path: &str) -> String {
    let mut out = String::from("file://");
    if !path.starts_with('/') {
        out.push('/');
    }
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// One source file's bytes, indexed by line, for byte → UTF-16 columns.
struct Source {
    bytes: Vec<u8>,
    line_starts: Vec<usize>,
}

impl Source {
    fn load(path: &Path) -> Option<Self> {
        let bytes = fs::read(path).ok()?;
        let mut line_starts = vec![0];
        line_starts.extend(bytes.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i + 1));
        Some(Self { bytes, line_starts })
    }

    /// The text of a single-line range given in 0-based byte columns.
    fn text(&self, line: u32, start: u32, end: u32) -> Option<String> {
        let from = *self.line_starts.get(line as usize - 1)? + start as usize;
        let to = (*self.line_starts.get(line as usize - 1)? + end as usize).min(self.bytes.len());
        let slice = self.bytes.get(from..to)?;
        Some(String::from_utf8_lossy(slice).into_owned())
    }

    /// Whether a single-line range in 1-based byte columns covers anything but a
    /// line break.
    fn covers_text(&self, line: u32, start_col: u32, end_col: u32) -> bool {
        self.text(line, start_col.saturating_sub(1), end_col.saturating_sub(1))
            .is_some_and(|t| !t.trim_end_matches(['\r', '\n']).is_empty())
    }

    /// The file's first character that is not white space: its 1-based line
    /// and the 1-based byte columns it spans.
    fn first_char(&self) -> Option<(u32, u32, u32)> {
        let offset = self.bytes.iter().position(|b| !b.is_ascii_whitespace())?;
        let width = match self.bytes[offset] {
            b if b >= 0xF0 => 4,
            b if b >= 0xE0 => 3,
            b if b >= 0xC0 => 2,
            _ => 1,
        };
        let line = self.line_starts.partition_point(|&start| start <= offset);
        let col = offset - self.line_starts[line - 1];
        Some((line as u32, col as u32 + 1, (col + width) as u32 + 1))
    }

    /// The UTF-16 column of a 0-based byte column on a 1-based line.
    fn utf16_col(&self, line: u32, byte_col: u32) -> u32 {
        let Some(&start) = self.line_starts.get(line as usize - 1) else {
            return byte_col;
        };
        let end = (start + byte_col as usize).min(self.bytes.len());
        let text = String::from_utf8_lossy(&self.bytes[start..end]);
        text.encode_utf16().count() as u32
    }
}

struct Builder<'g> {
    index: Index<'g>,
    options: &'g Options,
    unreadable: Vec<String>,
}

impl<'g> std::ops::Deref for Builder<'g> {
    type Target = Index<'g>;

    fn deref(&self) -> &Index<'g> {
        &self.index
    }
}

impl<'g> Builder<'g> {
    fn new(graph: &'g Graph, options: &'g Options) -> Self {
        Self { index: Index::build(graph), options, unreadable: Vec::new() }
    }

    fn warnings(&self) -> Vec<Warning> {
        let mut out = self.index.warnings("LSIF");
        if self.options.source_root.is_none() {
            out.push(Warning::new(
                "no-source-root",
                "no source root given: columns are emitted as byte offsets, which are wrong \
                 on lines holding non-ASCII text, and projectRoot is a placeholder",
            ));
        }
        if !self.unreadable.is_empty() {
            out.push(Warning::new(
                "unreadable-source",
                format!(
                    "{} file(s) could not be read to convert columns ({}); \
                     their columns stay byte offsets",
                    self.unreadable.len(),
                    self.unreadable.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
        let languages: HashSet<_> = self.docs.iter().map(|d| d.language).collect();
        if languages.len() > 1 {
            out.push(Warning::new(
                "multiple-languages",
                format!(
                    "{} languages in one dump; one LSIF project is written per language, \
                     which not every consumer accepts",
                    languages.len()
                ),
            ));
        }
        out
    }

    fn emit(&mut self, out: &mut dyn Write) -> Result<()> {
        let mut emitter = Emitter { out, next_id: 1 };
        let version = self.options.lsif_version.as_deref().unwrap_or(DEFAULT_VERSION);
        let root = self.options.source_root.as_ref().map_or_else(
            || "file:///project".to_owned(),
            |p| {
                let abs = p.canonicalize().unwrap_or_else(|_| p.clone());
                file_uri(&abs.to_string_lossy())
            },
        );

        emitter.vertex(
            "metaData",
            json!({
                "version": version,
                "projectRoot": root,
                "positionEncoding": "utf-16",
                "toolInfo": {"name": "callix", "version": self.options.tool_version},
            }),
        )?;

        // One project per language, in order of first appearance.
        let mut languages: Vec<&'static str> = Vec::new();
        for doc in &self.docs {
            if !languages.contains(&doc.language) {
                languages.push(doc.language);
            }
        }
        let mut project_ids: HashMap<&str, u64> = HashMap::new();
        for &lang in &languages {
            project_ids.insert(lang, emitter.vertex("project", json!({"kind": lang}))?);
        }

        // Per-symbol vertices, all up front: a range can name its resultSet
        // only once the resultSet exists.
        let mut sym_ids = Vec::with_capacity(self.symbols.len());
        for symbol in &self.symbols {
            let ids = emit_symbol(&mut emitter, symbol, self.symbol_language(symbol))?;
            sym_ids.push(ids);
        }
        // What a site-keyed external symbol is called is read off the source
        // while its document is written; its hover comes last.
        let mut site_names: HashMap<usize, String> = HashMap::new();

        let mut doc_ids: HashMap<&str, Vec<u64>> = HashMap::new();
        let mut unreadable: Vec<String> = Vec::new();

        for doc in &self.docs {
            if doc.definitions.is_empty() && doc.references.is_empty() {
                continue;
            }
            let source = match &self.options.source_root {
                Some(root) => {
                    let loaded = Source::load(&root.join(&doc.path));
                    if loaded.is_none() {
                        unreadable.push(doc.path.clone());
                    }
                    loaded
                }
                None => None,
            };
            let source = source.as_ref();

            let uri = match &self.options.source_root {
                Some(root) => {
                    let abs = root.canonicalize().unwrap_or_else(|_| root.clone());
                    file_uri(&format!("{}/{}", abs.to_string_lossy().trim_end_matches('/'), doc.path))
                }
                None => file_uri(&format!("/project/{}", doc.path)),
            };
            let doc_id = emitter.vertex(
                "document",
                json!({"uri": uri, "languageId": doc.language}),
            )?;
            doc_ids.entry(doc.language).or_default().push(doc_id);

            // Ranges, in the order definitions then references.
            let mut range_ids: Vec<u64> = Vec::new();
            let mut per_symbol: Vec<(usize, Vec<u64>, Vec<u64>)> = Vec::new();
            let mut slot: HashMap<usize, usize> = HashMap::new();

            for (is_def, ranges) in [(true, &doc.definitions), (false, &doc.references)] {
                for range in ranges {
                    if !is_def
                        && self.symbols[range.symbol].is_site_key()
                        && range.start.line == range.end.line
                        && !site_names.contains_key(&range.symbol)
                        && let Some(text) = source.and_then(|s| {
                            s.text(range.start.line, range.start.col - 1, range.end.col - 1)
                        })
                    {
                        site_names.insert(range.symbol, text);
                    }
                    let (mut start, mut end) = (range.start, range.end);
                    // A definition with nothing to show - a MODULE placed at 1:1 of a
                    // file that starts with a blank line - moves to the file's first
                    // character; an empty file has none and gets no range.
                    if is_def
                        && start.line == end.line
                        && let Some(source) = source
                        && !source.covers_text(start.line, start.col, end.col)
                    {
                        let Some((line, from, to)) = source.first_char() else {
                            continue;
                        };
                        start = Pos { line, col: from };
                        end = Pos { line, col: to };
                    }
                    let id = emitter.vertex(
                        "range",
                        json!({
                            "start": position(source, start),
                            "end": position(source, end),
                        }),
                    )?;
                    range_ids.push(id);
                    emitter.edge("next", id, &[sym_ids[range.symbol].result_set], None, None)?;
                    let at = *slot.entry(range.symbol).or_insert_with(|| {
                        per_symbol.push((range.symbol, Vec::new(), Vec::new()));
                        per_symbol.len() - 1
                    });
                    if is_def {
                        per_symbol[at].1.push(id);
                    } else {
                        per_symbol[at].2.push(id);
                    }
                }
            }
            emitter.edge("contains", doc_id, &range_ids, None, None)?;

            for (symbol, defs, refs) in &per_symbol {
                let ids = &sym_ids[*symbol];
                if !defs.is_empty() {
                    if let Some(definition_result) = ids.definition_result {
                        emitter.edge("item", definition_result, defs, Some(doc_id), None)?;
                    }
                    emitter.edge(
                        "item",
                        ids.reference_result,
                        defs,
                        Some(doc_id),
                        Some("definitions"),
                    )?;
                }
                if !refs.is_empty() {
                    emitter.edge(
                        "item",
                        ids.reference_result,
                        refs,
                        Some(doc_id),
                        Some("references"),
                    )?;
                }
            }
        }

        for lang in &languages {
            let docs = doc_ids.remove(lang).unwrap_or_default();
            if !docs.is_empty() {
                emitter.edge("contains", project_ids[lang], &docs, None, None)?;
            }
        }
        for (index, symbol) in self.symbols.iter().enumerate() {
            if !symbol.is_site_key() {
                continue;
            }
            let name = site_names.get(&index).map_or("symbol", String::as_str);
            let origin = symbol.node.meta_str("origin").unwrap_or("unknown");
            let language = self.symbol_language(symbol);
            let hover = emitter.vertex(
                "hoverResult",
                json!({"result": {"contents": [
                    {"language": language, "value": format!("{name}  (external, {origin})")}
                ]}}),
            )?;
            emitter.edge("textDocument/hover", sym_ids[index].result_set, &[hover], None, None)?;
        }
        self.unreadable = unreadable;
        Ok(())
    }

    /// An external symbol has no file of its own; borrow the first language
    /// that uses it, falling back to the first document's.
    fn symbol_language(&self, symbol: &Symbol<'_>) -> &'static str {
        if let Some(path) = &symbol.node.file_path {
            return language_id(path);
        }
        self.docs.first().map_or("plaintext", |d| d.language)
    }
}

struct SymbolIds {
    result_set: u64,
    definition_result: Option<u64>,
    reference_result: u64,
}

fn emit_symbol(emitter: &mut Emitter<'_>, symbol: &Symbol<'_>, language: &str) -> Result<SymbolIds> {
    let node = symbol.node;
    let result_set = emitter.vertex("resultSet", json!({}))?;

    if !symbol.is_site_key() {
        // The kind and name as code, then what the author wrote about it: a
        // plain string in a hover is Markdown.
        let mut contents = vec![
            json!({"language": language, "value": format!("{} {}", node.kind.as_str(), node.qualified_name)}),
        ];
        if let Some(doc) = node.meta_str("doc").filter(|doc| !doc.is_empty()) {
            contents.push(json!(doc));
        }
        let hover = emitter.vertex("hoverResult", json!({"result": {"contents": contents}}))?;
        emitter.edge("textDocument/hover", result_set, &[hover], None, None)?;
    }

    let definition_result = if symbol.external {
        None
    } else {
        let id = emitter.vertex("definitionResult", json!({}))?;
        emitter.edge("textDocument/definition", result_set, &[id], None, None)?;
        Some(id)
    };
    let reference_result = emitter.vertex("referenceResult", json!({}))?;
    emitter.edge("textDocument/references", result_set, &[reference_result], None, None)?;

    if symbol.external && !symbol.is_site_key() {
        let scheme = node.meta_str("origin").unwrap_or("callix");
        let moniker = emitter.vertex(
            "moniker",
            json!({
                "scheme": scheme,
                "identifier": node.qualified_name,
                "kind": "import",
                "unique": "scheme",
            }),
        )?;
        emitter.edge("moniker", result_set, &[moniker], None, None)?;
    }
    Ok(SymbolIds { result_set, definition_result, reference_result })
}

/// An LSP position from a 1-based line and 1-based byte column.
fn position(source: Option<&Source>, pos: Pos) -> Value {
    let byte_col = pos.col.saturating_sub(1);
    let character = source.map_or(byte_col, |s| s.utf16_col(pos.line, byte_col));
    json!({"line": pos.line.saturating_sub(1), "character": character})
}

/// Writes elements with ascending ids, one JSON object per line.
struct Emitter<'a> {
    out: &'a mut dyn Write,
    next_id: u64,
}

impl Emitter<'_> {
    fn vertex(&mut self, label: &str, fields: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let mut obj = Map::new();
        obj.insert("id".into(), json!(id));
        obj.insert("type".into(), json!("vertex"));
        obj.insert("label".into(), json!(label));
        if let Value::Object(fields) = fields {
            obj.extend(fields);
        }
        serde_json::to_writer(&mut *self.out, &Value::Object(obj))?;
        self.out.write_all(b"\n")?;
        Ok(id)
    }

    /// An edge from `out_v` to one or more vertices. A single target is written
    /// as `inV`, several as `inVs`, as the specification has it.
    fn edge(
        &mut self,
        label: &str,
        out_v: u64,
        in_vs: &[u64],
        document: Option<u64>,
        property: Option<&str>,
    ) -> Result<()> {
        if in_vs.is_empty() {
            return Ok(());
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut obj = Map::new();
        obj.insert("id".into(), json!(id));
        obj.insert("type".into(), json!("edge"));
        obj.insert("label".into(), json!(label));
        obj.insert("outV".into(), json!(out_v));
        // `item` and `contains` are 1:n and always spelled with inVs.
        if in_vs.len() == 1 && !matches!(label, "item" | "contains") {
            obj.insert("inV".into(), json!(in_vs[0]));
        } else {
            obj.insert("inVs".into(), json!(in_vs));
        }
        if let Some(document) = document {
            obj.insert("document".into(), json!(document));
        }
        if let Some(property) = property {
            obj.insert("property".into(), json!(property));
        }
        serde_json::to_writer(&mut *self.out, &Value::Object(obj))?;
        self.out.write_all(b"\n")?;
        Ok(())
    }
}

/// A structural check of an LSIF dump, for tests and for anyone producing or
/// post-processing one: the rules a consumer relies on, not the full schema.
pub mod check {
    use std::collections::{HashMap, HashSet};

    use serde_json::Value;

    /// What a well-formed dump contained.
    #[derive(Debug, Default)]
    pub struct Summary {
        pub vertices: usize,
        pub edges: usize,
        pub documents: usize,
        pub ranges: usize,
    }

    /// The result vertex an LSP request edge must lead to.
    fn result_label(edge: &str) -> Option<&'static str> {
        Some(match edge {
            "textDocument/definition" => "definitionResult",
            "textDocument/references" => "referenceResult",
            "textDocument/hover" => "hoverResult",
            _ => return None,
        })
    }

    /// Returns the summary, or every problem found (at most one per element).
    pub fn validate(text: &str) -> Result<Summary, Vec<String>> {
        let mut problems = Vec::new();
        let mut summary = Summary::default();
        let mut labels: HashMap<u64, String> = HashMap::new();
        let mut seen: HashSet<u64> = HashSet::new();
        let mut range_doc: HashMap<u64, u64> = HashMap::new();
        let mut last = 0u64;

        for (n, line) in text.lines().enumerate() {
            let n = n + 1;
            let el: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    problems.push(format!("line {n}: not JSON: {e}"));
                    continue;
                }
            };
            let Some(id) = el["id"].as_u64() else {
                problems.push(format!("line {n}: missing numeric id"));
                continue;
            };
            if id <= last {
                problems.push(format!("line {n}: id {id} does not ascend (after {last})"));
            }
            last = last.max(id);
            if !seen.insert(id) {
                problems.push(format!("line {n}: duplicate id {id}"));
            }
            let label = el["label"].as_str().unwrap_or("").to_owned();
            if n == 1 && label != "metaData" {
                problems.push("line 1: the first element must be metaData".into());
            }
            labels.insert(id, label.clone());

            match el["type"].as_str() {
                Some("vertex") => {
                    summary.vertices += 1;
                    match label.as_str() {
                        "document" => summary.documents += 1,
                        "range" => summary.ranges += 1,
                        _ => {}
                    }
                }
                Some("edge") => {
                    summary.edges += 1;
                    let Some(out_v) = el["outV"].as_u64().filter(|v| seen.contains(v)) else {
                        problems.push(format!("line {n}: {label}: outV is missing or not yet written"));
                        continue;
                    };
                    let targets: Vec<u64> = match (el.get("inV"), el.get("inVs")) {
                        (Some(v), None) => v.as_u64().into_iter().collect(),
                        (None, Some(vs)) => {
                            vs.as_array().map_or_else(Vec::new, |a| a.iter().filter_map(Value::as_u64).collect())
                        }
                        _ => {
                            problems.push(format!("line {n}: {label}: needs exactly one of inV / inVs"));
                            continue;
                        }
                    };
                    if targets.is_empty() {
                        problems.push(format!("line {n}: {label}: no targets"));
                        continue;
                    }
                    if let Some(t) = targets.iter().find(|t| !seen.contains(t)) {
                        problems.push(format!("line {n}: {label}: target {t} not yet written"));
                        continue;
                    }
                    let out_label = labels[&out_v].as_str();
                    match label.as_str() {
                        "contains" if out_label == "document" => {
                            for t in &targets {
                                if labels[t] != "range" {
                                    problems.push(format!("line {n}: a document contains a {}", labels[t]));
                                }
                                range_doc.insert(*t, out_v);
                            }
                        }
                        "contains" if out_label == "project" => {
                            if targets.iter().any(|t| labels[t] != "document") {
                                problems.push(format!("line {n}: a project contains a non-document"));
                            }
                        }
                        "next" => {
                            if out_label != "range" || labels[&targets[0]] != "resultSet" {
                                problems.push(format!("line {n}: next must go from a range to a resultSet"));
                            }
                        }
                        "item" => match el["document"].as_u64() {
                            Some(doc) if labels.get(&doc).map(String::as_str) == Some("document") => {
                                if let Some(t) = targets.iter().find(|t| range_doc.get(t) != Some(&doc)) {
                                    problems.push(format!("line {n}: item range {t} is not in document {doc}"));
                                }
                            }
                            _ => problems.push(format!("line {n}: item without a valid document")),
                        },
                        other => {
                            if let Some(expected) = result_label(other)
                                && labels[&targets[0]] != expected
                            {
                                problems.push(format!("line {n}: {other} must lead to a {expected}"));
                            }
                        }
                    }
                }
                other => problems.push(format!("line {n}: unknown type {other:?}")),
            }
        }
        if summary.vertices + summary.edges == 0 {
            problems.push("empty dump".into());
        }
        if problems.is_empty() { Ok(summary) } else { Err(problems) }
    }
}
