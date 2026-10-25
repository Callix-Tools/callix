//! The graph as JSON text, written without Python.
//!
//! `Graph.to_json()` goes through CPython's `json.dumps`; this is the same
//! document for the build that has no CPython, down to the bytes — the
//! separators, `ensure_ascii=False`, Python's spelling of a float — so that a
//! graph from the command line and one from the library are interchangeable
//! and a test can compare them as strings.

/// `SCHEMA_VERSION` of the serialized graph. It will not change within 1.x.
pub const SCHEMA_VERSION: u32 = 1;

#[cfg(not(feature = "python"))]
pub use native::graph_to_json;

#[cfg(not(feature = "python"))]
mod native {
    use std::fmt::Write as _;

    use crate::graph::Graph;
    use crate::node::Node;
    use crate::py::{Python, Value};
    use crate::relation::Relation;
    use crate::span::Span;

    use super::SCHEMA_VERSION;

    /// `repr(float)`: the shortest string that round-trips, in Python's layout —
    /// fixed notation for decimal exponents from -4 up to 15, otherwise
    /// `1e-05` / `1.5e+16`, always with a point or an exponent.
    pub(crate) fn float_repr(x: f64) -> String {
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
        // `{:e}` is Rust's shortest round-trip digits: "1.5e300", "9e-1".
        let scientific = format!("{:e}", x.abs());
        let (mantissa, exponent) = scientific.split_once('e').expect("{:e} has an exponent");
        let exponent: i32 = exponent.parse().expect("{:e} has a numeric exponent");
        let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
        // Where the decimal point falls, counted from the first digit.
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

    /// `json.dumps(..., ensure_ascii=False)`'s string escaping.
    fn push_string(out: &mut String, text: &str) {
        out.push('"');
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    /// Streams JSON with `json.dumps`'s layout for a given `indent`.
    struct Writer<'a> {
        out: &'a mut String,
        indent: Option<usize>,
        depth: usize,
    }

    impl Writer<'_> {
        fn newline(&mut self) {
            if let Some(width) = self.indent {
                self.out.push('\n');
                for _ in 0..width * self.depth {
                    self.out.push(' ');
                }
            }
        }

        fn open(&mut self, bracket: char) {
            self.out.push(bracket);
            self.depth += 1;
        }

        /// Before the next item: the separator, then the line break.
        fn item(&mut self, first: bool) {
            if !first {
                self.out.push_str(if self.indent.is_some() { "," } else { ", " });
            }
            self.newline();
        }

        fn close(&mut self, bracket: char, empty: bool) {
            self.depth -= 1;
            if !empty {
                self.newline();
            }
            self.out.push(bracket);
        }

        fn key(&mut self, name: &str) {
            push_string(self.out, name);
            self.out.push_str(": ");
        }

        fn string(&mut self, text: &str) {
            push_string(self.out, text);
        }

        fn int(&mut self, n: i64) {
            let _ = write!(self.out, "{n}");
        }

        fn spans(&mut self, span: Span) {
            self.open('[');
            for (i, n) in [span.start_line, span.start_col, span.end_line, span.end_col]
                .into_iter()
                .enumerate()
            {
                self.item(i == 0);
                self.int(i64::from(n));
            }
            self.close(']', false);
        }

        /// `encode_value`: the JSON form of a metadata value.
        fn value(&mut self, value: &Value) {
            match value {
                Value::None => self.out.push_str("null"),
                Value::Bool(b) => self.out.push_str(if *b { "true" } else { "false" }),
                Value::Int(n) => self.int(*n),
                Value::Float(x) => self.out.push_str(&float_repr(*x)),
                Value::Str(s) => self.string(s),
                Value::List(items) => {
                    self.open('[');
                    for (i, item) in items.iter().enumerate() {
                        self.item(i == 0);
                        self.value(item);
                    }
                    self.close(']', items.is_empty());
                }
                Value::Dict(map) => {
                    self.open('{');
                    for (i, (name, item)) in map.iter().enumerate() {
                        self.item(i == 0);
                        self.key(name);
                        self.value(item);
                    }
                    self.close('}', map.is_empty());
                }
                Value::Span(span) => {
                    self.open('{');
                    self.item(true);
                    self.key(crate::json::SPAN_TAG);
                    self.spans(*span);
                    self.close('}', false);
                }
            }
        }

        fn metadata(&mut self, entries: &[(String, Value)]) {
            self.open('{');
            for (i, (name, item)) in entries.iter().enumerate() {
                self.item(i == 0);
                self.key(name);
                self.value(item);
            }
            self.close('}', entries.is_empty());
        }

        fn node(&mut self, py: Python<'_>, node: &Node) {
            self.open('{');
            self.item(true);
            self.key("id");
            self.string(&node.id);
            self.item(false);
            self.key("kind");
            self.string(node.kind.as_str());
            self.item(false);
            self.key("qualified_name");
            self.string(&node.qualified_name);
            self.item(false);
            self.key("name");
            self.string(&node.name);
            self.item(false);
            self.key("file_path");
            match &node.file_path {
                Some(path) => self.string(path),
                None => self.out.push_str("null"),
            }
            self.item(false);
            self.key("span");
            match node.span {
                Some(span) => self.spans(span),
                None => self.out.push_str("null"),
            }
            self.item(false);
            self.key("metadata");
            self.metadata(&node.metadata.bind(py).entries());
            self.close('}', false);
        }

        fn relation(&mut self, py: Python<'_>, relation: &Relation) {
            self.open('{');
            self.item(true);
            self.key("source_id");
            self.string(&relation.source_id);
            self.item(false);
            self.key("target_id");
            self.string(&relation.target_id);
            self.item(false);
            self.key("kind");
            self.string(relation.kind.as_str());
            self.item(false);
            self.key("metadata");
            self.metadata(&relation.metadata.bind(py).entries());
            self.close('}', false);
        }
    }

    /// `graph.to_json(indent=indent)`, byte for byte.
    pub fn graph_to_json(py: Python<'_>, graph: &Graph, indent: Option<u32>) -> String {
        let mut text = String::new();
        let mut w = Writer { out: &mut text, indent: indent.map(|n| n as usize), depth: 0 };

        w.open('{');
        w.item(true);
        w.key("schema_version");
        w.int(i64::from(SCHEMA_VERSION));
        w.item(false);
        w.key("metadata");
        w.metadata(&graph.metadata_dict(py).entries());

        let nodes = graph.node_map();
        w.item(false);
        w.key("nodes");
        w.open('[');
        for (i, node) in nodes.values().enumerate() {
            w.item(i == 0);
            w.node(py, node.get());
        }
        w.close(']', nodes.is_empty());

        let relations = graph.relation_slice();
        w.item(false);
        w.key("relations");
        w.open('[');
        for (i, relation) in relations.iter().enumerate() {
            w.item(i == 0);
            w.relation(py, relation.get());
        }
        w.close(']', relations.is_empty());
        w.close('}', false);
        text
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn floats_are_spelled_as_python_does() {
            // Each pair was produced by CPython's `repr`.
            let table: [(f64, &str); 29] = [
                (0.0, "0.0"), (-0.0, "-0.0"), (1.0, "1.0"), (0.9, "0.9"), (0.1, "0.1"),
                (0.5, "0.5"), (100.0, "100.0"), (1000000000000000.0, "1000000000000000.0"),
                (1e+16, "1e+16"), (1.5e+16, "1.5e+16"), (123456789012345.0, "123456789012345.0"),
                (1234567890123456.0, "1234567890123456.0"),
                (1.2345678901234568e+16, "1.2345678901234568e+16"), (0.0001, "0.0001"),
                (1e-05, "1e-05"), (0.00012345, "0.00012345"), (1.5e-07, "1.5e-07"),
                (1e+300, "1e+300"), (1.7976931348623157e+308, "1.7976931348623157e+308"),
                (5e-324, "5e-324"), (1.2345678901234567, "1.2345678901234567"),
                (0.30000000000000004, "0.30000000000000004"), (2.5, "2.5"), (89.3, "89.3"),
                (100.0, "100.0"), (0.034, "0.034"), (176.0, "176.0"), (1e+22, "1e+22"),
                (123.456, "123.456"),
            ];
            for (value, expected) in table {
                assert_eq!(float_repr(value), expected, "{value:e}");
            }
            assert_eq!(float_repr(-2.5), "-2.5");
            assert_eq!(float_repr(-1e-7), "-1e-07");
        }

        #[test]
        fn strings_escape_like_json_dumps_without_ascii_forcing() {
            let mut out = String::new();
            push_string(&mut out, "a\"b\\c\nd\te\u{1}é€\u{7f}");
            assert_eq!(out, "\"a\\\"b\\\\c\\nd\\te\\u0001é€\u{7f}\"");
        }
    }
}

/// The key a `Span` stored in metadata is wrapped in.
#[cfg(not(feature = "python"))]
pub(crate) const SPAN_TAG: &str = "__span__";
