//! What both index formats are made of: documents, the definitions and
//! references in them, and the symbols those point at.
//!
//! LSIF and SCIP are two encodings of the same thing, so the reading of the
//! graph — which nodes are definitions, which edges are placed references, what
//! is skipped and why — lives here once.

use std::collections::{HashMap, HashSet};

use crate::Warning;
use crate::model::{Graph, Node, NodeKind, RelationKind, Span};

/// Node kinds that are definitions with a position of their own.
pub(crate) const DEFINITION_KINDS: [NodeKind; 8] = [
    NodeKind::Module,
    NodeKind::Class,
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Parameter,
    NodeKind::Variable,
    NodeKind::Attribute,
    NodeKind::TypeAlias,
];

/// The LSIF language id for a path, by extension.
pub fn language_id(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map_or("", |(_, ext)| ext);
    match ext.to_ascii_lowercase().as_str() {
        "py" | "pyi" => "python",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "go" => "go",
        "rs" => "rust",
        "php" => "php",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "c++" | "h++" => "cpp",
        "yaml" | "yml" => "yaml",
        _ => "plaintext",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Pos {
    pub line: u32,
    pub col: u32,
}

pub(crate) struct Range {
    pub start: Pos,
    pub end: Pos,
    pub symbol: usize,
    /// The whole extent of a definition (`Node.span`), for formats that keep it.
    pub extent: Option<Span>,
    /// How a reference touches its target: `read` or `write`, when recorded.
    pub access: Access,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Access {
    #[default]
    Unknown,
    Read,
    Write,
}

#[derive(Default)]
pub(crate) struct Doc {
    pub path: String,
    pub language: &'static str,
    pub definitions: Vec<Range>,
    pub references: Vec<Range>,
}

pub(crate) struct Symbol<'g> {
    pub node: &'g Node,
    pub external: bool,
}

impl Symbol<'_> {
    /// A resolver that could not name its target leaves a node keyed by the
    /// site — `call@pkg/models.py:8:29` — which says where, not what. Such a
    /// node is one use-site, so it is named after the text found there.
    pub(crate) fn is_site_key(&self) -> bool {
        self.external && is_site_key(&self.node.qualified_name)
    }
}

fn is_site_key(qualified_name: &str) -> bool {
    let Some((_, site)) = qualified_name.rsplit_once('@') else { return false };
    let mut parts = site.rsplitn(3, ':');
    let (col, line) = (parts.next(), parts.next());
    parts.next().is_some()
        && col.is_some_and(|c| c.parse::<u32>().is_ok())
        && line.is_some_and(|l| l.parse::<u32>().is_ok())
}

/// A graph read as an index.
pub(crate) struct Index<'g> {
    pub graph: &'g Graph,
    pub docs: Vec<Doc>,
    pub doc_index: HashMap<String, usize>,
    pub symbols: Vec<Symbol<'g>>,
    pub symbol_index: HashMap<&'g str, usize>,
    pub skipped_definitions: usize,
    pub skipped_references: usize,
    pub unplaced_references: usize,
    pub ref_edges: usize,
}

impl<'g> Index<'g> {
    pub fn build(graph: &'g Graph) -> Self {
        let mut index = Self {
            graph,
            docs: Vec::new(),
            doc_index: HashMap::new(),
            symbols: Vec::new(),
            symbol_index: HashMap::new(),
            skipped_definitions: 0,
            skipped_references: 0,
            unplaced_references: 0,
            ref_edges: 0,
        };
        index.collect();
        index
    }

    fn doc(&mut self, path: &str) -> usize {
        if let Some(&i) = self.doc_index.get(path) {
            return i;
        }
        let i = self.docs.len();
        self.docs.push(Doc { path: path.to_owned(), language: language_id(path), ..Doc::default() });
        self.doc_index.insert(path.to_owned(), i);
        i
    }

    fn symbol(&mut self, node: &'g Node, external: bool) -> usize {
        if let Some(&i) = self.symbol_index.get(node.id.as_str()) {
            return i;
        }
        let i = self.symbols.len();
        self.symbols.push(Symbol { node, external });
        self.symbol_index.insert(node.id.as_str(), i);
        i
    }

    /// Reads the graph into per-document definitions and references.
    fn collect(&mut self) {
        let graph = self.graph;

        // Documents first, in the order the graph lists its files, so the
        // output order is a function of the graph and nothing else.
        for node in graph.nodes_of(NodeKind::File) {
            if let Some(path) = &node.file_path {
                self.doc(path);
            }
        }

        for node in &graph.nodes {
            if !DEFINITION_KINDS.contains(&node.kind) {
                continue;
            }
            let (Some(path), Some(name_span)) = (&node.file_path, node.name_span()) else {
                // A module with a file but no name span (YAML, Go) is just not placed.
                if node.file_path.is_some() && node.kind != NodeKind::Module {
                    self.skipped_definitions += 1;
                }
                continue;
            };
            let symbol = self.symbol(node, false);
            let doc = self.doc(path);
            let mut range = range_of(name_span, symbol);
            range.extent = node.span;
            self.docs[doc].definitions.push(range);
        }

        let mut seen: HashSet<(usize, Span, usize)> = HashSet::new();
        for relation in &graph.relations {
            // A RESOLVES_TO that carries a span is the name an import brings in, as
            // the resolver answered it; the structural ones carry no metadata.
            let placed_import =
                matches!(relation.kind, RelationKind::ResolvesTo) && relation.span().is_some();
            if !relation.kind.is_reference() && !placed_import {
                continue;
            }
            self.ref_edges += 1;
            let Some(span) = relation.span() else {
                self.unplaced_references += 1;
                continue;
            };
            let (Some(source), Some(target)) =
                (graph.node(&relation.source_id), graph.node(&relation.target_id))
            else {
                self.skipped_references += 1;
                continue;
            };
            // The position belongs to the file the edge was written in, which
            // is not the source node's file when several definitions share a
            // qualified name and so are one node.
            let Some(path) = relation.site_file().or(source.file_path.as_deref()) else {
                self.skipped_references += 1;
                continue;
            };
            let symbol = match target.kind {
                NodeKind::ExternalSymbol => self.symbol(target, true),
                kind if DEFINITION_KINDS.contains(&kind) => {
                    match self.symbol_index.get(target.id.as_str()) {
                        Some(&i) => i,
                        None => {
                            // A definition without a name span: nothing to
                            // navigate to, so the reference cannot be placed
                            // on a symbol either.
                            self.skipped_references += 1;
                            continue;
                        }
                    }
                }
                _ => {
                    self.skipped_references += 1;
                    continue;
                }
            };
            let doc = self.doc(path);
            if !seen.insert((doc, span, symbol)) {
                continue;
            }
            // A write to a name at its own definition resolves to itself.
            if self.docs[doc]
                .definitions
                .iter()
                .any(|d| d.symbol == symbol && range_of(span, symbol).start == d.start)
            {
                continue;
            }
            let mut range = range_of(span, symbol);
            range.access = match relation.metadata.get("access").and_then(|v| v.as_str()) {
                Some("read") => Access::Read,
                Some("write") => Access::Write,
                _ => Access::Unknown,
            };
            self.docs[doc].references.push(range);
        }
    }

}

pub(crate) fn range_of(span: Span, symbol: usize) -> Range {
    Range {
        start: Pos { line: span.start_line, col: span.start_col },
        end: Pos { line: span.end_line, col: span.end_col },
        symbol,
        extent: None,
        access: Access::Unknown,
    }
}

impl Index<'_> {
    /// The warnings about what the graph could not give an index, whatever its
    /// format. `format` names it in the message.
    pub fn warnings(&self, format: &str) -> Vec<Warning> {
        let mut out = Vec::new();
        match self.graph.resolver_status() {
            Some("ok") => {}
            Some(status) => out.push(Warning::new(
                "resolver-status",
                format!(
                    "the graph's resolver status is '{status}': references and definitions \
                     reached through symbol resolution may be missing from the index"
                ),
            )),
            None => out.push(Warning::new(
                "resolver-status",
                "the graph records no resolver status; completeness of references is unknown",
            )),
        }
        if self.ref_edges == 0 {
            out.push(Warning::new(
                "no-references",
                "the graph has no resolved references (was it built with resolve=False?); \
                 the index holds definitions only, so 'find references' will be empty",
            ));
        }
        if self.skipped_definitions > 0 {
            out.push(Warning::new(
                "no-name-span",
                format!("{} definition(s) carry no name span and were left out", self.skipped_definitions),
            ));
        }
        if self.unplaced_references > 0 {
            out.push(Warning::new(
                "no-reference-span",
                format!("{} reference edge(s) record no position and were left out", self.unplaced_references),
            ));
        }
        if self.skipped_references > 0 {
            out.push(Warning::new(
                "unsupported-reference",
                format!(
                    "{} reference(s) point at a node {format} has no definition for \
                     (a module, a dependency, or a definition without a name span)",
                    self.skipped_references
                ),
            ));
        }
        out
    }
}
