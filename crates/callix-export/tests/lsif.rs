mod common;

use callix_export::lsif::Lsif;
use callix_export::model::Graph;
use callix_export::{Exporter, Options};
use common::{C_DECL_SITES, MODULE_GRAPH, SAMPLE, check_lsif, edges, labelled};

fn export(options: &Options) -> (String, Vec<callix_export::Warning>) {
    let graph = Graph::from_json(SAMPLE).unwrap();
    let mut out = Vec::new();
    let warnings = Lsif.export(&graph, &mut out, options).unwrap();
    (String::from_utf8(out).unwrap(), warnings)
}

#[test]
fn output_is_well_formed_lsif() {
    let (text, _) = export(&Options::default());
    check_lsif(&text);
}

#[test]
fn documents_ranges_and_results() {
    let (text, _) = export(&Options { tool_version: "9.9".into(), ..Options::default() });
    let el = check_lsif(&text);

    let meta = &el[0];
    assert_eq!(meta["version"], "0.4.3");
    assert_eq!(meta["toolInfo"]["name"], "callix");
    assert_eq!(meta["toolInfo"]["version"], "9.9");

    // a.py and b.py have ranges; both are documents
    let docs = labelled(&el, "document");
    assert_eq!(docs.len(), 2);
    assert!(docs[0]["uri"].as_str().unwrap().ends_with("/a.py"));
    assert_eq!(docs[0]["languageId"], "python");

    // definitions: charge, rate (a.py); checkout, refund (b.py) = 4;
    // references: charge x2 distinct sites (dup collapsed), httpx.get, rate = 4
    assert_eq!(labelled(&el, "range").len(), 8);

    // three internal symbols with a definition result + the variable: charge, rate,
    // checkout, refund -> 4 definitionResults; httpx.get has none
    assert_eq!(labelled(&el, "definitionResult").len(), 4);
    assert_eq!(labelled(&el, "referenceResult").len(), 5);
    assert_eq!(labelled(&el, "resultSet").len(), 5);

    // the external symbol becomes an import moniker
    let monikers = labelled(&el, "moniker");
    assert_eq!(monikers.len(), 1);
    assert_eq!(monikers[0]["identifier"], "httpx.get");
    assert_eq!(monikers[0]["scheme"], "third_party");
    assert_eq!(monikers[0]["kind"], "import");
}

#[test]
fn positions_are_zero_based() {
    let (text, _) = export(&Options::default());
    let el = check_lsif(&text);
    // charge's name_span is 1:5-1:11 -> line 0, characters 4..10
    let hit = labelled(&el, "range").into_iter().any(|r| {
        r["start"] == serde_json::json!({"line": 0, "character": 4})
            && r["end"] == serde_json::json!({"line": 0, "character": 10})
    });
    assert!(hit, "definition of charge not found at 0:4-0:10");
}

#[test]
fn references_are_listed_per_document() {
    let (text, _) = export(&Options::default());
    let el = check_lsif(&text);
    let items = edges(&el, "item");
    let refs: Vec<_> = items.iter().filter(|i| i["property"] == "references").collect();
    // charge is referenced from b.py (two sites), httpx.get from b.py, rate from b.py
    let total: usize = refs.iter().map(|i| i["inVs"].as_array().unwrap().len()).sum();
    assert_eq!(total, 4);
}

#[test]
fn warns_when_unresolved_and_without_root() {
    let unresolved = SAMPLE.replace(r#""resolver_status": "ok""#, r#""resolver_status": "degraded""#);
    let graph = Graph::from_json(&unresolved).unwrap();
    let mut out = Vec::new();
    let warnings = Lsif.export(&graph, &mut out, &Options::default()).unwrap();
    let codes: Vec<_> = warnings.iter().map(|w| w.code).collect();
    assert!(codes.contains(&"resolver-status"));
    assert!(codes.contains(&"no-source-root"));
}

#[test]
fn structural_graph_exports_definitions_and_warns() {
    let mut value: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
    let relations = value["relations"].as_array_mut().unwrap();
    relations.retain(|r| r["metadata"].get("span").is_none());
    value["metadata"]["resolver_status"] = "unavailable".into();
    let graph = Graph::from_value(value).unwrap();
    let mut out = Vec::new();
    let warnings = Lsif.export(&graph, &mut out, &Options::default()).unwrap();
    let el = check_lsif(&String::from_utf8(out).unwrap());
    assert_eq!(labelled(&el, "range").len(), 4);
    assert!(warnings.iter().any(|w| w.code == "no-references"));
}

#[test]
fn columns_become_utf16_when_the_source_is_available() {
    let dir = std::env::temp_dir().join(format!("callix-lsif-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // "é" is 2 bytes in UTF-8 and 1 unit in UTF-16; the name starts after it.
    std::fs::write(dir.join("a.py"), "def é_charge(): pass\n").unwrap();
    std::fs::write(dir.join("b.py"), "x = 1\n").unwrap();
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"fa","kind":"file","qualified_name":"a.py","name":"a.py","file_path":"a.py","span":null,"metadata":{}},
       {"id":"f","kind":"function","qualified_name":"a.é_charge","name":"é_charge","file_path":"a.py","span":[1,1,1,21],
        "metadata":{"name_span":{"__span__":[1,5,1,15]}}}],
      "relations":[]}"#;
    let graph = Graph::from_json(json).unwrap();
    let mut out = Vec::new();
    Lsif.export(&graph, &mut out, &Options { source_root: Some(dir.clone()), ..Options::default() })
        .unwrap();
    let el = check_lsif(&String::from_utf8(out).unwrap());
    let range = labelled(&el, "range")[0];
    // byte columns 4..14 ("é_charge" is 9 bytes: 2 + 7 ... start 4, end 14)
    assert_eq!(range["start"]["character"], 4);
    // 10 bytes between col 5 and 15 are 9 UTF-16 units: end = 4 + 9 = 13
    assert_eq!(range["end"]["character"], 13);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn site_keyed_external_symbols_are_named_from_the_source() {
    let dir = std::env::temp_dir().join(format!("callix-lsif-site-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.py"), "def f():\n    return int(1)\n").unwrap();
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"fa","kind":"file","qualified_name":"a.py","name":"a.py","file_path":"a.py","span":null,"metadata":{}},
       {"id":"f","kind":"function","qualified_name":"a.f","name":"f","file_path":"a.py","span":[1,1,2,16],
        "metadata":{"name_span":{"__span__":[1,5,1,6]}}},
       {"id":"e","kind":"external_symbol","qualified_name":"call@a.py:2:12","name":"py:2:12","file_path":null,"span":null,
        "metadata":{"origin":"stdlib"}}],
      "relations":[{"source_id":"f","target_id":"e","kind":"calls","metadata":{"span":{"__span__":[2,12,2,15]}}}]}"#;
    let graph = Graph::from_json(json).unwrap();
    let mut out = Vec::new();
    Lsif.export(&graph, &mut out, &Options { source_root: Some(dir.clone()), ..Options::default() })
        .unwrap();
    let el = check_lsif(&String::from_utf8(out).unwrap());
    // No moniker for a node that is only a site, and the hover names the callee.
    assert!(labelled(&el, "moniker").is_empty());
    let hovers: Vec<String> = labelled(&el, "hoverResult")
        .iter()
        .map(|h| h["result"]["contents"][0]["value"].as_str().unwrap().to_owned())
        .collect();
    assert!(hovers.iter().any(|h| h == "int  (external, stdlib)"), "{hovers:?}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_reference_goes_to_the_file_it_was_written_in() {
    // Two definitions that share a qualified name are one node, which lives in
    // the first file. An edge from it can be written in the second, and the
    // position has to land there, not in the first file's document.
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"fa","kind":"file","qualified_name":"a.go","name":"a.go","file_path":"a.go","span":null,"metadata":{}},
       {"id":"fb","kind":"file","qualified_name":"b.go","name":"b.go","file_path":"b.go","span":null,"metadata":{}},
       {"id":"init","kind":"function","qualified_name":"p.init","name":"init","file_path":"a.go","span":[3,1,5,2],
        "metadata":{"name_span":{"__span__":[3,6,3,10]}}},
       {"id":"set","kind":"function","qualified_name":"p.set","name":"set","file_path":"a.go","span":[7,1,9,2],
        "metadata":{"name_span":{"__span__":[7,6,7,9]}}}],
      "relations":[
       {"source_id":"init","target_id":"set","kind":"calls","metadata":{"span":{"__span__":[4,2,4,5]}}},
       {"source_id":"init","target_id":"set","kind":"calls","metadata":{"span":{"__span__":[40,2,40,5]},"file":"b.go"}}]}"#;
    let graph = Graph::from_json(json).unwrap();
    let mut out = Vec::new();
    Lsif.export(&graph, &mut out, &Options::default()).unwrap();
    let el = check_lsif(&String::from_utf8(out).unwrap());

    let uri = |id: &serde_json::Value| {
        labelled(&el, "document")
            .iter()
            .find(|d| &d["id"] == id)
            .map(|d| d["uri"].as_str().unwrap().rsplit('/').next().unwrap().to_owned())
    };
    let line_of = |range: &serde_json::Value| range["start"]["line"].as_u64().unwrap();
    for contains in edges(&el, "contains") {
        let Some(doc) = uri(&contains["outV"]) else { continue };
        for id in contains["inVs"].as_array().unwrap() {
            let range = el.iter().find(|e| &e["id"] == id && e["label"] == "range").unwrap();
            // Line 41 (zero-based 39) exists in b.go only; a.go has nine lines.
            assert_eq!(line_of(range) == 39, doc == "b.go", "{doc}: line {}", line_of(range));
        }
    }
}

#[test]
fn a_hover_carries_what_the_author_wrote() {
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"fa","kind":"file","qualified_name":"a.py","name":"a.py","file_path":"a.py","span":null,"metadata":{}},
       {"id":"f","kind":"function","qualified_name":"a.f","name":"f","file_path":"a.py","span":[1,1,2,8],
        "metadata":{"name_span":{"__span__":[1,5,1,6]},"doc":"Does the thing.\n\nCarefully."}},
       {"id":"g","kind":"function","qualified_name":"a.g","name":"g","file_path":"a.py","span":[4,1,5,8],
        "metadata":{"name_span":{"__span__":[4,5,4,6]}}}],
      "relations":[]}"#;
    let graph = Graph::from_json(json).unwrap();
    let mut out = Vec::new();
    Lsif.export(&graph, &mut out, &Options::default()).unwrap();
    let el = check_lsif(&String::from_utf8(out).unwrap());
    let contents: Vec<&serde_json::Value> = labelled(&el, "hoverResult")
        .iter()
        .map(|h| &h["result"]["contents"])
        .collect();
    // Documented: the kind and name as code, then the text as Markdown.
    assert!(
        contents.iter().any(|c| c[0]["value"] == "function a.f" && c[1] == "Does the thing.\n\nCarefully."),
        "{contents:?}"
    );
    // Undocumented: the kind and name alone.
    assert!(contents.iter().any(|c| c[0]["value"] == "function a.g" && c.as_array().unwrap().len() == 1));
}

#[test]
fn a_go_package_graph_is_valid_lsif() {
    let graph = Graph::from_json(common::GO_PACKAGE_GRAPH).unwrap();
    let mut out = Vec::new();
    Lsif.export(&graph, &mut out, &Options::default()).unwrap();
    let elements = check_lsif(&String::from_utf8(out).unwrap());
    // the package definition, the import path, the qualifier and `fmt`
    assert!(labelled(&elements, "range").len() >= 4);
}

#[test]
fn a_resolves_to_with_a_position_is_a_reference() {
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"fl","kind":"file","qualified_name":"src/lib.rs","name":"lib.rs","file_path":"src/lib.rs","span":null,"metadata":{}},
       {"id":"fs","kind":"file","qualified_name":"src/shapes.rs","name":"shapes.rs","file_path":"src/shapes.rs","span":null,"metadata":{}},
       {"id":"imp","kind":"import","qualified_name":"src/lib.rs::crate::shapes::Circle","name":"crate::shapes::Circle","file_path":"src/lib.rs","span":[1,1,1,31],"metadata":{}},
       {"id":"c","kind":"class","qualified_name":"demo::shapes::Circle","name":"Circle","file_path":"src/shapes.rs","span":[1,1,1,19],"metadata":{"name_span":{"__span__":[1,12,1,18]}}}],
      "relations":[
       {"source_id":"imp","target_id":"c","kind":"resolves_to","metadata":{}},
       {"source_id":"imp","target_id":"c","kind":"resolves_to","metadata":{"span":{"__span__":[1,24,1,30]},"resolution":"name"}}]}"#;
    let graph = Graph::from_json(json).unwrap();
    let mut out = Vec::new();
    Lsif.export(&graph, &mut out, &Options::default()).unwrap();
    let el = check_lsif(&String::from_utf8(out).unwrap());
    assert!(
        labelled(&el, "document").iter().any(|d| d["uri"].as_str().unwrap().ends_with("src/lib.rs")),
        "src/lib.rs has a document"
    );
    let hit = labelled(&el, "range").into_iter().any(|r| {
        r["start"] == serde_json::json!({"line": 0, "character": 23})
    });
    assert!(hit, "the leaf's range is not exported");
    let refs: usize = edges(&el, "item")
        .iter()
        .filter(|i| i["property"] == "references")
        .map(|i| i["inVs"].as_array().unwrap().len())
        .sum();
    assert_eq!(refs, 1);
}

#[test]
fn declaration_sites_and_includes_are_ranges() {
    let graph = Graph::from_json(C_DECL_SITES).unwrap();
    let mut out = Vec::new();
    Lsif.export(&graph, &mut out, &Options::default()).unwrap();
    let el = check_lsif(&String::from_utf8(out).unwrap());
    let ranges = labelled(&el, "range");
    let at = |line: u64, ch: u64| {
        ranges
            .iter()
            .filter(|r| r["start"]["line"] == line && r["start"]["character"] == ch)
            .count()
    };
    // `add`: its definition in a.c and its prototype in a.h.
    assert_eq!(at(2, 4), 2);
    // The a.h operand of the include in a.c.
    assert!(at(0, 9) >= 1);
    // The header's file symbol is defined on the first character of its own document.
    assert!(ranges.iter().any(|r| r["start"]["line"] == 0 && r["start"]["character"] == 0 && r["end"]["character"] == 1));
}

#[test]
fn a_module_exports_on_its_first_character() {
    let mut out = Vec::new();
    let graph = Graph::from_json(MODULE_GRAPH).unwrap();
    let warnings = Lsif.export(&graph, &mut out, &Options::default()).unwrap();
    check_lsif(&String::from_utf8(out).unwrap());
    assert!(!warnings.iter().any(|w| w.code == "no-name-span"));
}
