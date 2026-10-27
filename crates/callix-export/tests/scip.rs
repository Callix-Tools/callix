mod common;

use callix_export::model::Graph;
use callix_export::scip::Scip as Exporter;
use callix_export::{Exporter as _, Options};
use common::{MODULE_GRAPH, SAMPLE, decode_scip};

fn export(graph: &Graph) -> (Vec<u8>, Vec<callix_export::Warning>) {
    let mut out = Vec::new();
    let warnings = Exporter.export(graph, &mut out, &Options { tool_version: "9.9".into(), ..Options::default() }).unwrap();
    (out, warnings)
}

#[test]
fn metadata_and_documents() {
    let (bytes, _) = export(&Graph::from_json(SAMPLE).unwrap());
    let index = decode_scip(&bytes);
    assert_eq!(index.tool, ("callix".to_owned(), "9.9".to_owned()));
    assert_eq!(index.text_encoding, 1, "UTF-8");
    let paths: Vec<_> = index.documents.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(paths, ["a.py", "b.py"]);
    for doc in &index.documents {
        assert_eq!(doc.language, "python");
        assert_eq!(doc.position_encoding, 1, "UTF-8 code units from the line start");
    }
}

#[test]
fn definitions_carry_their_name_range_and_enclosing_extent() {
    let (bytes, _) = export(&Graph::from_json(SAMPLE).unwrap());
    let index = decode_scip(&bytes);
    let a = &index.documents[0];
    let charge = a
        .occurrences
        .iter()
        .find(|o| o.symbol == "callix . shop . . `a.charge`().")
        .expect("charge is defined in a.py");
    assert_eq!(charge.roles, 1, "Definition");
    // name_span 1:5-1:11 -> 0-based [line, start, end]
    assert_eq!(charge.range, [0, 4, 10]);
    // span 1:1-2:12 spans two lines -> [line, start, line, end]
    assert_eq!(charge.enclosing, [0, 0, 1, 11]);
    assert!(a.symbols.iter().any(|(s, kind, docs)| s.contains("charge") && *kind == 17 && docs[0] == "function a.charge"));
}

#[test]
fn references_carry_their_role_and_occurrences_are_sorted() {
    let (bytes, _) = export(&Graph::from_json(SAMPLE).unwrap());
    let index = decode_scip(&bytes);
    let b = &index.documents[1];
    // b.py: checkout and refund are defined; charge (twice, one site deduplicated),
    // rate (a read) and httpx.get are referenced.
    let refs: Vec<_> = b.occurrences.iter().filter(|o| o.roles & 1 == 0).collect();
    assert_eq!(refs.len(), 4, "{refs:?}");
    let rate = refs.iter().find(|o| o.symbol.contains("a.rate")).unwrap();
    assert_eq!(rate.roles, 8, "the sample records a read");
    let call = refs.iter().find(|o| o.symbol.contains("a.charge")).unwrap();
    assert_eq!(call.roles, 0, "a call has no read/write role");
    let mut sorted: Vec<_> = b.occurrences.iter().map(|o| o.range.clone()).collect();
    let before = sorted.clone();
    sorted.sort();
    assert_eq!(before, sorted);
    // an external symbol that is not defined in the graph is declared as external
    assert!(index.external.iter().any(|s| s == "callix . third_party . . `httpx.get`."), "{:?}", index.external);
}

#[test]
fn access_becomes_a_read_or_write_role() {
    let mut value: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
    for r in value["relations"].as_array_mut().unwrap() {
        if r["kind"] == "references" {
            r["metadata"]["access"] = "write".into();
        }
    }
    let (bytes, _) = export(&Graph::from_value(value).unwrap());
    let index = decode_scip(&bytes);
    let rate = index.documents[1].occurrences.iter().find(|o| o.symbol.contains("a.rate") && o.roles & 1 == 0).unwrap();
    assert_eq!(rate.roles, 4, "WriteAccess");

    let (bytes, _) = export(&Graph::from_json(SAMPLE).unwrap());
    let index = decode_scip(&bytes);
    // the sample's REFERENCES edge has access = read
    let read = index.documents[1].occurrences.iter().find(|o| o.symbol.contains("a.rate") && o.roles & 1 == 0).unwrap();
    assert_eq!(read.roles, 8, "ReadAccess");
}

#[test]
fn names_that_are_not_identifiers_are_backtick_escaped() {
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"p","kind":"project","qualified_name":"my project","name":"my project","file_path":null,"span":null,"metadata":{}},
       {"id":"fa","kind":"file","qualified_name":"a.go","name":"a.go","file_path":"a.go","span":null,"metadata":{}},
       {"id":"f","kind":"class","qualified_name":"ex/it`s.Thing","name":"Thing","file_path":"a.go","span":[1,1,1,20],"metadata":{"name_span":{"__span__":[1,6,1,11]}}}],
      "relations":[]}"#;
    let (bytes, _) = export(&Graph::from_json(json).unwrap());
    let symbol = &decode_scip(&bytes).documents[0].occurrences[0].symbol;
    assert_eq!(symbol, "callix . my  project . . `ex/it``s.Thing`#");
}

#[test]
fn site_keyed_externals_are_left_out_with_a_warning() {
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"fa","kind":"file","qualified_name":"a.py","name":"a.py","file_path":"a.py","span":null,"metadata":{}},
       {"id":"f","kind":"function","qualified_name":"a.f","name":"f","file_path":"a.py","span":[1,1,2,16],"metadata":{"name_span":{"__span__":[1,5,1,6]}}},
       {"id":"e","kind":"external_symbol","qualified_name":"call@a.py:2:12","name":"py:2:12","file_path":null,"span":null,"metadata":{"origin":"stdlib"}}],
      "relations":[{"source_id":"f","target_id":"e","kind":"calls","metadata":{"span":{"__span__":[2,12,2,15]}}}]}"#;
    let (bytes, warnings) = export(&Graph::from_json(json).unwrap());
    let index = decode_scip(&bytes);
    assert_eq!(index.documents[0].occurrences.len(), 1, "only the definition");
    assert!(warnings.iter().any(|w| w.code == "unnamed-external"));
}

/// rust-analyzer's own index for the Rust fixture is the reference: every
/// definition callix indexes must sit exactly where rust-analyzer says that
/// name is defined — same file, same 0-based identifier range.
#[test]
fn definitions_agree_with_rust_analyzers_index() {
    let graph = Graph::from_json(include_str!("data/rust-fixture.graph.json")).unwrap();
    let (ours, _) = export(&graph);
    let ours = decode_scip(&ours);
    let reference = decode_scip(include_bytes!("data/rust-fixture.rust-analyzer.scip"));

    // The reference really is a SCIP index in the shape the decoder expects.
    assert_eq!(reference.tool.0, "rust-analyzer");
    assert_eq!(reference.text_encoding, 1);
    assert!(reference.documents.iter().all(|d| d.position_encoding == 1));

    let mut compared = 0;
    for doc in &ours.documents {
        let theirs = reference
            .documents
            .iter()
            .find(|d| d.path == doc.path)
            .unwrap_or_else(|| panic!("rust-analyzer has no {}", doc.path));
        let defined_by_them: Vec<&Vec<u64>> =
            theirs.occurrences.iter().filter(|o| o.roles & 1 == 1).map(|o| &o.range).collect();
        for occurrence in doc.occurrences.iter().filter(|o| o.roles & 1 == 1) {
            assert!(
                defined_by_them.contains(&&occurrence.range),
                "{}: callix defines {} at {:?}, rust-analyzer has no definition there",
                doc.path,
                occurrence.symbol,
                occurrence.range
            );
            compared += 1;
        }
    }
    assert!(compared >= 15, "only {compared} definitions compared");
}

#[test]
fn a_structural_graph_still_exports_its_definitions() {
    let mut value: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
    value["relations"].as_array_mut().unwrap().retain(|r| r["metadata"].get("span").is_none());
    value["metadata"]["resolver_status"] = "unavailable".into();
    let (bytes, warnings) = export(&Graph::from_value(value).unwrap());
    let index = decode_scip(&bytes);
    assert!(index.documents.iter().all(|d| d.occurrences.iter().all(|o| o.roles == 1)));
    assert!(warnings.iter().any(|w| w.code == "no-references"));
}

#[test]
fn symbol_documentation_carries_what_the_author_wrote() {
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"fa","kind":"file","qualified_name":"a.py","name":"a.py","file_path":"a.py","span":null,"metadata":{}},
       {"id":"f","kind":"function","qualified_name":"a.f","name":"f","file_path":"a.py","span":[1,1,2,8],
        "metadata":{"name_span":{"__span__":[1,5,1,6]},"doc":"Does the thing."}},
       {"id":"g","kind":"function","qualified_name":"a.g","name":"g","file_path":"a.py","span":[4,1,5,8],
        "metadata":{"name_span":{"__span__":[4,5,4,6]}}}],
      "relations":[]}"#;
    let (bytes, warnings) = export(&Graph::from_json(json).unwrap());
    let index = decode_scip(&bytes);
    let docs = |name: &str| -> Vec<String> {
        index
            .documents
            .iter()
            .flat_map(|d| d.symbols.iter())
            .find(|(symbol, _, _)| symbol.contains(name))
            .map(|(_, _, docs)| docs.clone())
            .unwrap_or_default()
    };
    // The signature-like line first, then the author's text.
    assert_eq!(docs("`a.f`"), ["function a.f", "Does the thing."]);
    assert_eq!(docs("`a.g`"), ["function a.g"]);
    // Something in the graph is documented, so there is nothing to warn about.
    assert!(warnings.iter().all(|w| w.code != "no-documentation"), "{warnings:?}");
}

#[test]
fn a_placed_module_is_a_definition_and_a_reference_target() {
    let (bytes, warnings) = export(&Graph::from_json(MODULE_GRAPH).unwrap());
    let index = decode_scip(&bytes);
    let util = index.documents.iter().find(|d| d.path == "pkg/util.py").unwrap();
    let defs: Vec<_> = util.occurrences.iter().filter(|o| o.roles & 1 == 1).collect();
    assert_eq!(defs.len(), 1, "{defs:?}");
    assert_eq!(defs[0].range, vec![0, 0, 0]);
    assert!(defs[0].symbol.contains("pkg.util"), "{}", defs[0].symbol);
    assert!(
        util.symbols.iter().any(|(s, kind, _)| s.contains("pkg.util") && *kind == 29),
        "{:?}",
        util.symbols
    );
    let main = index.documents.iter().find(|d| d.path == "pkg/main.py").unwrap();
    let mut ranges: Vec<_> = main
        .occurrences
        .iter()
        .filter(|o| o.roles & 1 == 0 && o.symbol.contains("pkg.util"))
        .map(|o| o.range.clone())
        .collect();
    ranges.sort();
    assert_eq!(ranges, vec![vec![0, 16, 20], vec![3, 11, 15]]);
    for code in ["unnamed-external", "no-name-span", "unsupported-reference"] {
        assert!(!warnings.iter().any(|w| w.code == code), "unexpected warning {code}");
    }
}
