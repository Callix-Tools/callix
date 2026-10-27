#![allow(dead_code)] // each test binary uses a different part

//! Shared by the integration tests.

use serde_json::Value;

pub const SAMPLE: &str = include_str!("../data/sample.json");

/// A placed MODULE (`pkg.util`), a reference and a resolved import both pointing
/// at it, a YAML-like module with a file but no name span, and an unplaced one.
pub const MODULE_GRAPH: &str = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
  "nodes":[
   {"id":"p","kind":"project","qualified_name":"proj","name":"proj","file_path":null,"span":null,"metadata":{}},
   {"id":"fu","kind":"file","qualified_name":"pkg/util.py","name":"util.py","file_path":"pkg/util.py","span":null,"metadata":{}},
   {"id":"fm","kind":"file","qualified_name":"pkg/main.py","name":"main.py","file_path":"pkg/main.py","span":null,"metadata":{}},
   {"id":"fy","kind":"file","qualified_name":"ci.yaml","name":"ci.yaml","file_path":"ci.yaml","span":null,"metadata":{}},
   {"id":"mu","kind":"module","qualified_name":"pkg.util","name":"util","file_path":"pkg/util.py","span":[1,1,1,1],"metadata":{"name_span":{"__span__":[1,1,1,1]}}},
   {"id":"my","kind":"module","qualified_name":"ci","name":"ci","file_path":"ci.yaml","span":null,"metadata":{}},
   {"id":"mn","kind":"module","qualified_name":"ns","name":"ns","file_path":null,"span":null,"metadata":{}},
   {"id":"run","kind":"function","qualified_name":"pkg.main.run","name":"run","file_path":"pkg/main.py","span":[3,1,4,20],"metadata":{"name_span":{"__span__":[3,5,3,8]}}}],
  "relations":[
   {"source_id":"run","target_id":"mu","kind":"references","metadata":{"span":{"__span__":[4,12,4,16]},"access":"read"}},
   {"source_id":"run","target_id":"mu","kind":"resolves_to","metadata":{"span":{"__span__":[1,17,1,21]}}},
   {"source_id":"run","target_id":"mu","kind":"resolves_to","metadata":{}}]}"#;

/// Parses JSON Lines, asserts the dump is structurally valid and returns it.
pub fn check_lsif(text: &str) -> Vec<Value> {
    if let Err(problems) = callix_export::lsif::check::validate(text) {
        panic!("invalid LSIF:\n  {}", problems.join("\n  "));
    }
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

pub fn labelled<'a>(elements: &'a [Value], label: &str) -> Vec<&'a Value> {
    elements.iter().filter(|e| e["type"] == "vertex" && e["label"] == label).collect()
}

pub fn edges<'a>(elements: &'a [Value], label: &str) -> Vec<&'a Value> {
    elements.iter().filter(|e| e["type"] == "edge" && e["label"] == label).collect()
}

// ---------------------------------------------------------------------------
// A protobuf reader for SCIP, independent of the encoder under test.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Wire {
    Varint(u64),
    Bytes(Vec<u8>),
}

fn varint(bytes: &[u8], at: &mut usize) -> u64 {
    let (mut value, mut shift) = (0u64, 0);
    loop {
        let byte = bytes[*at];
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
        shift += 7;
    }
}

pub fn fields(bytes: &[u8]) -> Vec<(u32, Wire)> {
    let (mut at, mut out) = (0, Vec::new());
    while at < bytes.len() {
        let key = varint(bytes, &mut at);
        let (field, wire) = ((key >> 3) as u32, key & 7);
        match wire {
            0 => out.push((field, Wire::Varint(varint(bytes, &mut at)))),
            2 => {
                let len = varint(bytes, &mut at) as usize;
                out.push((field, Wire::Bytes(bytes[at..at + len].to_vec())));
                at += len;
            }
            other => panic!("unexpected wire type {other}"),
        }
    }
    out
}

fn text(wire: &Wire) -> String {
    match wire {
        Wire::Bytes(b) => String::from_utf8(b.clone()).unwrap(),
        Wire::Varint(_) => panic!("expected a string"),
    }
}

fn packed(wire: &Wire) -> Vec<u64> {
    let Wire::Bytes(b) = wire else { panic!("expected packed ints") };
    let (mut at, mut out) = (0, Vec::new());
    while at < b.len() {
        out.push(varint(b, &mut at));
    }
    out
}

#[derive(Debug, Default)]
pub struct Occ {
    pub range: Vec<u64>,
    pub symbol: String,
    pub roles: u64,
    pub enclosing: Vec<u64>,
}

#[derive(Debug, Default)]
pub struct Doc {
    pub path: String,
    pub language: String,
    pub position_encoding: u64,
    pub occurrences: Vec<Occ>,
    pub symbols: Vec<(String, u64, Vec<String>)>,
}

#[derive(Debug, Default)]
pub struct Scip {
    pub tool: (String, String),
    pub project_root: String,
    pub text_encoding: u64,
    pub documents: Vec<Doc>,
    pub external: Vec<String>,
}

pub fn decode_scip(bytes: &[u8]) -> Scip {
    let mut index = Scip::default();
    for (field, value) in fields(bytes) {
        match (field, &value) {
            (1, Wire::Bytes(meta)) => {
                for (f, v) in fields(meta) {
                    match (f, &v) {
                        (2, Wire::Bytes(tool)) => {
                            for (tf, tv) in fields(tool) {
                                match tf {
                                    1 => index.tool.0 = text(&tv),
                                    2 => index.tool.1 = text(&tv),
                                    _ => {}
                                }
                            }
                        }
                        (3, _) => index.project_root = text(&v),
                        (4, Wire::Varint(n)) => index.text_encoding = *n,
                        _ => {}
                    }
                }
            }
            (2, Wire::Bytes(doc)) => {
                let mut d = Doc::default();
                for (f, v) in fields(doc) {
                    match (f, &v) {
                        (1, _) => d.path = text(&v),
                        (4, _) => d.language = text(&v),
                        (6, Wire::Varint(n)) => d.position_encoding = *n,
                        (2, Wire::Bytes(occ)) => {
                            let mut o = Occ::default();
                            for (of, ov) in fields(occ) {
                                match (of, &ov) {
                                    (1, _) => o.range = packed(&ov),
                                    (2, _) => o.symbol = text(&ov),
                                    (3, Wire::Varint(n)) => o.roles = *n,
                                    (7, _) => o.enclosing = packed(&ov),
                                    _ => {}
                                }
                            }
                            d.occurrences.push(o);
                        }
                        (3, Wire::Bytes(info)) => {
                            let (mut symbol, mut kind, mut docs) = (String::new(), 0, Vec::new());
                            for (sf, sv) in fields(info) {
                                match (sf, &sv) {
                                    (1, _) => symbol = text(&sv),
                                    (3, _) => docs.push(text(&sv)),
                                    (5, Wire::Varint(n)) => kind = *n,
                                    _ => {}
                                }
                            }
                            d.symbols.push((symbol, kind, docs));
                        }
                        _ => {}
                    }
                }
                index.documents.push(d);
            }
            (3, Wire::Bytes(info)) => {
                for (sf, sv) in fields(info) {
                    if sf == 1 {
                        index.external.push(text(&sv));
                    }
                }
            }
            _ => {}
        }
    }
    index
}
