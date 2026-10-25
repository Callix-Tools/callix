//! Whole-graph operations on the JSON model: write it back out, merge two,
//! diff two. They follow `Graph.to_json`, `Graph.merge` and `Graph.diff` of the
//! library, so a saved graph behaves the same wherever it is handled.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use serde_json::ser::Formatter;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::model::{Graph, Metadata, Node, Relation, SCHEMA_VERSION, Span};

// ---------------------------------------------------------------------------
// JSON in the layout of Python's `json.dumps`
// ---------------------------------------------------------------------------

/// `repr(float)`. The graph's own floats (confidence, seconds, percentages) are
/// ordinary numbers; the exponent forms exist for completeness.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x < 0.0 { "-Infinity" } else { "Infinity" }.into();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let scientific = format!("{:e}", x.abs());
    let (mantissa, exponent) = scientific.split_once('e').expect("{:e} has an exponent");
    let exponent: i32 = exponent.parse().expect("{:e} has a numeric exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let point = exponent + 1;
    let len = digits.len() as i32;
    if (-3..=16).contains(&point) {
        if point <= 0 {
            format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
        } else if point >= len {
            format!("{sign}{digits}{}.0", "0".repeat((point - len) as usize))
        } else {
            let (whole, fraction) = digits.split_at(point as usize);
            format!("{sign}{whole}.{fraction}")
        }
    } else {
        let (first, rest) = digits.split_at(1);
        let tail = if rest.is_empty() { String::new() } else { format!(".{rest}") };
        let esign = if exponent < 0 { '-' } else { '+' };
        format!("{sign}{first}{tail}e{esign}{:02}", exponent.abs())
    }
}

/// `json.dumps` without an indent: `", "` between items and `": "` after keys.
struct PyCompact;

impl Formatter for PyCompact {
    fn begin_array_value<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }

    fn begin_object_key<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b": ")
    }

    fn write_f64<W: ?Sized + Write>(&mut self, w: &mut W, value: f64) -> std::io::Result<()> {
        w.write_all(float_repr(value).as_bytes())
    }
}

/// The same with an indent, which `serde_json` already lays out like Python.
struct PyPretty<'a>(serde_json::ser::PrettyFormatter<'a>);

impl Formatter for PyPretty<'_> {
    fn begin_array<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_array(w)
    }
    fn end_array<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_array(w)
    }
    fn begin_array_value<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        self.0.begin_array_value(w, first)
    }
    fn end_array_value<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_array_value(w)
    }
    fn begin_object<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_object(w)
    }
    fn end_object<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_object(w)
    }
    fn begin_object_key<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        self.0.begin_object_key(w, first)
    }
    fn begin_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_object_value(w)
    }
    fn end_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_object_value(w)
    }
    fn write_f64<W: ?Sized + Write>(&mut self, w: &mut W, value: f64) -> std::io::Result<()> {
        w.write_all(float_repr(value).as_bytes())
    }
}

fn span_list(span: Option<Span>) -> Value {
    span.map_or(Value::Null, |s| {
        Value::Array(
            [s.start_line, s.start_col, s.end_line, s.end_col].map(Value::from).to_vec(),
        )
    })
}

struct NodeOut<'a>(&'a Node);

impl Serialize for NodeOut<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let n = self.0;
        let mut map = s.serialize_map(Some(7))?;
        map.serialize_entry("id", &n.id)?;
        map.serialize_entry("kind", n.kind.as_str())?;
        map.serialize_entry("qualified_name", &n.qualified_name)?;
        map.serialize_entry("name", &n.name)?;
        map.serialize_entry("file_path", &n.file_path)?;
        map.serialize_entry("span", &span_list(n.span))?;
        map.serialize_entry("metadata", &n.metadata)?;
        map.end()
    }
}

struct RelationOut<'a>(&'a Relation);

impl Serialize for RelationOut<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let r = self.0;
        let mut map = s.serialize_map(Some(4))?;
        map.serialize_entry("source_id", &r.source_id)?;
        map.serialize_entry("target_id", &r.target_id)?;
        map.serialize_entry("kind", r.kind.as_str())?;
        map.serialize_entry("metadata", &r.metadata)?;
        map.end()
    }
}

struct GraphOut<'a>(&'a Graph);

impl Serialize for GraphOut<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        struct Nodes<'a>(&'a [Node]);
        struct Relations<'a>(&'a [Relation]);
        impl Serialize for Nodes<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                s.collect_seq(self.0.iter().map(NodeOut))
            }
        }
        impl Serialize for Relations<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                s.collect_seq(self.0.iter().map(RelationOut))
            }
        }
        let g = self.0;
        let mut map = s.serialize_map(Some(4))?;
        map.serialize_entry("schema_version", &SCHEMA_VERSION)?;
        map.serialize_entry("metadata", &g.metadata)?;
        map.serialize_entry("nodes", &Nodes(&g.nodes))?;
        map.serialize_entry("relations", &Relations(&g.relations))?;
        map.end()
    }
}

impl Graph {
    /// The graph as JSON, laid out the way `Graph.to_json(indent=indent)` is.
    pub fn write_json(&self, out: &mut dyn Write, indent: Option<usize>) -> Result<()> {
        match indent {
            None => {
                let mut ser = serde_json::Serializer::with_formatter(&mut *out, PyCompact);
                GraphOut(self).serialize(&mut ser)?;
            }
            Some(width) => {
                let pad = vec![b' '; width];
                let formatter = PyPretty(serde_json::ser::PrettyFormatter::with_indent(&pad));
                let mut ser = serde_json::Serializer::with_formatter(&mut *out, formatter);
                GraphOut(self).serialize(&mut ser)?;
            }
        }
        Ok(())
    }

    pub fn to_json(&self, indent: Option<usize>) -> Result<String> {
        let mut buffer = Vec::new();
        self.write_json(&mut buffer, indent)?;
        Ok(String::from_utf8(buffer).expect("serde_json writes UTF-8"))
    }

    // -----------------------------------------------------------------------
    // merge
    // -----------------------------------------------------------------------

    /// Merges `other` in, as `Graph.merge` does.
    ///
    /// `allow_shared` lets *identical* nodes coincide — how graphs of different
    /// languages meet at a shared BOUNDARY — while two different nodes with one
    /// id stay an error. Either the whole graph merges or nothing does, and
    /// relations are appended without deduplication: merge once per source.
    pub fn merge(&mut self, other: &Graph, allow_shared: bool) -> Result<()> {
        for node in &other.nodes {
            if let Some(existing) = self.node(&node.id) {
                if allow_shared && same_node(existing, node) {
                    continue;
                }
                return Err(Error::Invalid(format!("Node with id '{}' already exists", node.id)));
            }
        }
        for node in &other.nodes {
            if self.node(&node.id).is_none() {
                self.push_node_unchecked(node.clone());
            }
        }
        self.relations.extend(other.relations.iter().cloned());

        // Resolver statuses merge to the worst, or a degraded side would be
        // masked by whichever graph came last.
        let before = self.resolver_status().map(str::to_owned);
        let incoming = other.resolver_status().map(str::to_owned);
        for (key, value) in &other.metadata {
            self.metadata.insert(key.clone(), value.clone());
        }
        if let (Some(a), Some(b)) = (before, incoming) {
            let worst = if severity(&a) >= severity(&b) { a } else { b };
            self.metadata.insert("resolver_status".into(), Value::String(worst));
        }
        Ok(())
    }
}

fn severity(status: &str) -> u8 {
    match status {
        "ok" => 0,
        "degraded" => 1,
        // "unavailable" and anything unrecognised count as the worst.
        _ => 2,
    }
}

fn same_node(a: &Node, b: &Node) -> bool {
    a.id == b.id
        && a.kind == b.kind
        && a.qualified_name == b.qualified_name
        && a.name == b.name
        && a.file_path == b.file_path
        && a.span == b.span
        && a.metadata == b.metadata
}

// ---------------------------------------------------------------------------
// diff
// ---------------------------------------------------------------------------

/// A relation's identity: the node pair, the kind and its canonical metadata,
/// so two call sites stay two edges and a metadata-only change is noticed.
type RelationKey = (String, String, &'static str, String);

fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, String> = map.iter().map(|(k, v)| (k, canonical(v))).collect();
            let body: Vec<String> = sorted.into_iter().map(|(k, v)| format!("{k}:{v}")).collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", body.join(","))
        }
        other => other.to_string(),
    }
}

fn relation_key(r: &Relation) -> RelationKey {
    let metadata: Metadata = r.metadata.clone();
    (r.source_id.clone(), r.target_id.clone(), r.kind.as_str(), canonical(&Value::Object(metadata)))
}

/// The structural difference between two graphs.
#[derive(Debug, Default)]
pub struct Diff<'a> {
    pub added_nodes: Vec<&'a Node>,
    pub removed_nodes: Vec<&'a Node>,
    pub changed_nodes: Vec<(&'a Node, &'a Node)>,
    pub added_relations: Vec<&'a Relation>,
    pub removed_relations: Vec<&'a Relation>,
}

impl Diff<'_> {
    pub fn is_empty(&self) -> bool {
        self.added_nodes.is_empty()
            && self.removed_nodes.is_empty()
            && self.changed_nodes.is_empty()
            && self.added_relations.is_empty()
            && self.removed_relations.is_empty()
    }
}

/// The diff from `old` to `new`. Nodes are listed sorted by id, relations by key.
pub fn diff<'a>(old: &'a Graph, new: &'a Graph) -> Diff<'a> {
    let mut added: Vec<&Node> = Vec::new();
    let mut changed: Vec<(&Node, &Node)> = Vec::new();
    for node in &new.nodes {
        match old.node(&node.id) {
            None => added.push(node),
            Some(previous) if !same_node(previous, node) => changed.push((previous, node)),
            Some(_) => {}
        }
    }
    let mut removed: Vec<&Node> = old.nodes.iter().filter(|n| new.node(&n.id).is_none()).collect();
    added.sort_by(|a, b| a.id.cmp(&b.id));
    removed.sort_by(|a, b| a.id.cmp(&b.id));
    changed.sort_by(|a, b| a.0.id.cmp(&b.0.id));

    let keyed = |g: &'a Graph| -> BTreeMap<RelationKey, &'a Relation> {
        g.relations.iter().map(|r| (relation_key(r), r)).collect()
    };
    let (old_rels, new_rels) = (keyed(old), keyed(new));
    let old_keys: HashSet<&RelationKey> = old_rels.keys().collect();
    let new_keys: HashSet<&RelationKey> = new_rels.keys().collect();

    Diff {
        added_nodes: added,
        removed_nodes: removed,
        changed_nodes: changed,
        added_relations: new_rels.iter().filter(|(k, _)| !old_keys.contains(k)).map(|(_, r)| *r).collect(),
        removed_relations: old_rels.iter().filter(|(k, _)| !new_keys.contains(k)).map(|(_, r)| *r).collect(),
    }
}
