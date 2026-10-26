//! SCIP — Sourcegraph's code intelligence protocol, the protobuf successor of
//! LSIF.
//!
//! The same index as the LSIF dump, in SCIP's shape: one `Document` per file
//! holding its `Occurrence`s (a definition carries its enclosing extent, a
//! reference its read/write role) and a `SymbolInformation` per symbol defined
//! there. Positions are 0-based line and **UTF-8 byte offset** from the start of
//! the line — callix's own columns, so no source files are needed and every
//! column is exact.
//!
//! A symbol is `callix . <project> . . <descriptor>`: the qualified name as one
//! (backtick-escaped) descriptor whose suffix says what it is — `#` for a type,
//! `().` for a callable, `.` for a term, `(…)` for a parameter. External symbols
//! that already are SCIP symbols (rust-analyzer's, kept verbatim in the graph)
//! are used as they are; ones the resolver could not name — a node keyed by the
//! site it was met at — have no identity to index and are left out.
//!
//! The encoder is hand-written protobuf (varints and length-delimited fields),
//! checked against an index rust-analyzer produced; field numbers are those of
//! `scip.proto`.

use std::io::Write;

use crate::error::Result;
use crate::index::{Access, Index, Range, Symbol};
use crate::model::{Graph, NodeKind, Span};
use crate::{Exporter, Options, Warning};

pub struct Scip;

impl Exporter for Scip {
    fn name(&self) -> &'static str {
        "scip"
    }

    fn description(&self) -> &'static str {
        "SCIP index (protobuf), for Sourcegraph and other SCIP consumers"
    }

    fn extension(&self) -> &'static str {
        "scip"
    }

    fn binary(&self) -> bool {
        true
    }

    fn export(
        &self,
        graph: &Graph,
        out: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Warning>> {
        let index = Index::build(graph);
        let mut warnings = index.warnings("SCIP");

        let project = graph
            .nodes_of(NodeKind::Project)
            .next()
            .map_or("unknown", |p| p.name.as_str());
        let symbols: Vec<Option<String>> =
            index.symbols.iter().map(|s| symbol_string(project, s)).collect();

        let mut metadata = Vec::new();
        let mut tool = Vec::new();
        put_str(&mut tool, 1, "callix");
        put_str(&mut tool, 2, &options.tool_version);
        put_bytes(&mut metadata, 2, &tool);
        let root = options.source_root.as_ref().map_or_else(
            || "file:///project".to_owned(),
            |p| format!("file://{}", p.canonicalize().unwrap_or_else(|_| p.clone()).display()),
        );
        put_str(&mut metadata, 3, &root);
        put_varint_field(&mut metadata, 4, TEXT_ENCODING_UTF8);
        let mut body = Vec::new();
        put_bytes(&mut body, 1, &metadata);
        out.write_all(&body)?;

        // A symbol defined somewhere in the graph needs no entry under
        // `external_symbols`; the rest, referenced from here, do.
        let mut defined = vec![false; index.symbols.len()];
        let mut dropped_sites = 0usize;

        for doc in &index.docs {
            if doc.definitions.is_empty() && doc.references.is_empty() {
                continue;
            }
            let mut occurrences: Vec<(&Range, bool)> = Vec::new();
            for range in &doc.definitions {
                defined[range.symbol] = true;
                occurrences.push((range, true));
            }
            for range in &doc.references {
                if symbols[range.symbol].is_none() {
                    dropped_sites += 1;
                    continue;
                }
                occurrences.push((range, false));
            }
            occurrences.sort_by_key(|(r, _)| (r.start.line, r.start.col, r.end.line, r.end.col));

            let mut message = Vec::new();
            put_str(&mut message, 1, &doc.path);
            for (range, is_definition) in &occurrences {
                let Some(symbol) = &symbols[range.symbol] else { continue };
                put_bytes(&mut message, 2, &occurrence(range, symbol, *is_definition));
            }
            let mut seen = std::collections::HashSet::new();
            for range in &doc.definitions {
                if !seen.insert(range.symbol) {
                    continue;
                }
                if let Some(symbol) = &symbols[range.symbol] {
                    put_bytes(&mut message, 3, &symbol_information(&index.symbols[range.symbol], symbol));
                }
            }
            put_str(&mut message, 4, doc.language);
            put_varint_field(&mut message, 6, POSITION_UTF8);

            let mut field = Vec::new();
            put_bytes(&mut field, 2, &message);
            out.write_all(&field)?;
        }

        for (i, symbol) in index.symbols.iter().enumerate() {
            if defined[i] || !symbol.external {
                continue;
            }
            if let Some(text) = &symbols[i] {
                let mut field = Vec::new();
                put_bytes(&mut field, 3, &symbol_information(symbol, text));
                out.write_all(&field)?;
            }
        }

        if dropped_sites > 0 {
            warnings.push(Warning::new(
                "unnamed-external",
                format!(
                    "{dropped_sites} reference(s) to symbols the resolver could not name \
                     (an external symbol keyed by its call site) were left out"
                ),
            ));
        }
        if !index.symbols.iter().any(|s| s.node.meta_str("doc").is_some()) {
            warnings.push(Warning::new(
                "no-documentation",
                "no definition in the graph carries documentation (a graph from before callix \
                 recorded it, or code without docstrings or comments); each symbol's \
                 documentation is its kind and qualified name",
            ));
        }
        Ok(warnings)
    }
}

// ---------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------

/// A name as one descriptor: bare when it is made of identifier characters,
/// otherwise in backticks with any backtick doubled.
fn escape_name(name: &str) -> String {
    let bare = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-' | '$'));
    if bare { name.to_owned() } else { format!("`{}`", name.replace('`', "``")) }
}

/// A space inside a package name is written as two.
fn escape_package(name: &str) -> String {
    if name.is_empty() { ".".into() } else { name.replace(' ', "  ") }
}

/// The SCIP symbol of a definition or an external symbol; none for a node that
/// has no identity (see the module docs).
fn symbol_string(project: &str, symbol: &Symbol<'_>) -> Option<String> {
    let node = symbol.node;
    if symbol.external {
        if symbol.is_site_key() {
            return None;
        }
        // rust-analyzer's symbols are kept verbatim in the graph.
        if node.qualified_name.split(' ').count() >= 5 {
            return Some(node.qualified_name.clone());
        }
        let origin = node.meta_str("origin").unwrap_or("external");
        return Some(format!(
            "callix . {} . . {}.",
            escape_package(origin),
            escape_name(&node.qualified_name)
        ));
    }
    let name = escape_name(&node.qualified_name);
    let descriptor = match node.kind {
        NodeKind::Class | NodeKind::TypeAlias => format!("{name}#"),
        NodeKind::Function | NodeKind::Method => format!("{name}()."),
        NodeKind::Parameter => format!("({name})"),
        NodeKind::Module | NodeKind::File => format!("{name}/"),
        _ => format!("{name}."),
    };
    Some(format!("callix . {} . . {descriptor}", escape_package(project)))
}

/// `SymbolInformation.Kind` for a node kind.
fn symbol_kind(kind: NodeKind) -> u64 {
    match kind {
        NodeKind::Class => 7,
        NodeKind::Function => 17,
        NodeKind::Method => 26,
        NodeKind::Module => 29,
        NodeKind::Parameter => 37,
        NodeKind::Variable => 61,
        NodeKind::Attribute => 15, // Field
        NodeKind::TypeAlias => 55,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

const TEXT_ENCODING_UTF8: u64 = 1;
/// `PositionEncoding.UTF8CodeUnitOffsetFromLineStart`.
const POSITION_UTF8: u64 = 1;
const ROLE_DEFINITION: u64 = 0x1;
const ROLE_WRITE: u64 = 0x4;
const ROLE_READ: u64 = 0x8;

/// `[line, start, end]`, or `[start line, start, end line, end]` across lines —
/// 0-based, from callix's 1-based line and column.
fn scip_range(start: (u32, u32), end: (u32, u32)) -> Vec<u64> {
    let (sl, sc) = (u64::from(start.0.saturating_sub(1)), u64::from(start.1.saturating_sub(1)));
    let (el, ec) = (u64::from(end.0.saturating_sub(1)), u64::from(end.1.saturating_sub(1)));
    if sl == el { vec![sl, sc, ec] } else { vec![sl, sc, el, ec] }
}

fn occurrence(range: &Range, symbol: &str, is_definition: bool) -> Vec<u8> {
    let mut out = Vec::new();
    put_packed(
        &mut out,
        1,
        &scip_range((range.start.line, range.start.col), (range.end.line, range.end.col)),
    );
    put_str(&mut out, 2, symbol);
    let roles = if is_definition {
        ROLE_DEFINITION
    } else {
        match range.access {
            Access::Write => ROLE_WRITE,
            Access::Read => ROLE_READ,
            Access::Unknown => 0,
        }
    };
    if roles != 0 {
        put_varint_field(&mut out, 3, roles);
    }
    if let Some(Span { start_line, start_col, end_line, end_col }) = range.extent.filter(|_| is_definition) {
        put_packed(&mut out, 7, &scip_range((start_line, start_col), (end_line, end_col)));
    }
    out
}

fn symbol_information(symbol: &Symbol<'_>, text: &str) -> Vec<u8> {
    let node = symbol.node;
    let mut out = Vec::new();
    put_str(&mut out, 1, text);
    put_str(&mut out, 3, &format!("{} {}", node.kind.as_str(), node.qualified_name));
    // `documentation` is repeated: the signature-like line, then the author's.
    if let Some(doc) = node.meta_str("doc").filter(|doc| !doc.is_empty()) {
        put_str(&mut out, 3, doc);
    }
    let kind = symbol_kind(node.kind);
    if kind != 0 {
        put_varint_field(&mut out, 5, kind);
    }
    put_str(&mut out, 6, &node.name);
    out
}

// ---------------------------------------------------------------------------
// Protobuf wire format
// ---------------------------------------------------------------------------

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value & 0x7f) as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_key(out: &mut Vec<u8>, field: u32, wire: u8) {
    put_varint(out, (u64::from(field) << 3) | u64::from(wire));
}

fn put_varint_field(out: &mut Vec<u8>, field: u32, value: u64) {
    put_key(out, field, 0);
    put_varint(out, value);
}

fn put_bytes(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    put_key(out, field, 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn put_str(out: &mut Vec<u8>, field: u32, text: &str) {
    put_bytes(out, field, text.as_bytes());
}

/// A packed `repeated int32` (proto3's default encoding).
fn put_packed(out: &mut Vec<u8>, field: u32, values: &[u64]) {
    let mut packed = Vec::new();
    for value in values {
        put_varint(&mut packed, *value);
    }
    put_bytes(out, field, &packed);
}
