mod common;

use callix_export::model::Graph;
use callix_export::scip::Scip as Exporter;
use callix_export::{Exporter as _, Options};
use common::{C_DECL_SITES, MODULE_GRAPH, SAMPLE, decode_scip};

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
fn a_go_package_is_a_definition_and_its_qualifier_and_import_path_reference_it() {
    let (bytes, warnings) = export(&Graph::from_json(common::GO_PACKAGE_GRAPH).unwrap());
    let index = decode_scip(&bytes);

    let util = index.documents.iter().find(|d| d.path == "util/util.go").expect("util/util.go");
    let definition = util
        .occurrences
        .iter()
        .find(|o| o.symbol.contains("example.com/m/util"))
        .expect("the package is defined at its package clause");
    assert_eq!(definition.roles, 1, "Definition");
    // name_span 1:9-1:13 -> 0-based [line, start, end]
    assert_eq!(definition.range, [0, 8, 12]);
    assert!(util.symbols.iter().any(|(s, kind, _)| s.contains("example.com/m/util") && *kind == 29));

    let main = index.documents.iter().find(|d| d.path == "main.go").expect("main.go");
    let refs: Vec<_> = main
        .occurrences
        .iter()
        .filter(|o| o.symbol.contains("example.com/m/util") && o.roles & 1 == 0)
        .map(|o| o.range.clone())
        .collect();
    // the import path (3:8-3:28) and the qualifier (6:6-6:10)
    assert!(refs.iter().any(|r| *r == [2, 7, 27]), "{refs:?}");
    assert!(refs.iter().any(|r| *r == [5, 5, 9]), "{refs:?}");
    assert!(main.occurrences.iter().any(|o| o.symbol.contains("fmt") && o.roles & 1 == 0));
    for code in ["no-name-span", "unsupported-reference", "unnamed-external"] {
        assert!(warnings.iter().all(|w| w.code != code), "{code}: {warnings:?}");
    }
}

/// A file that only re-exports has no definition. Its `use` leaf is a RESOLVES_TO with a
/// position, which is a reference occurrence, so the file still gets a document.
const PUB_USE_ONLY: &str = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
  "nodes":[
   {"id":"fl","kind":"file","qualified_name":"src/lib.rs","name":"lib.rs","file_path":"src/lib.rs","span":null,"metadata":{}},
   {"id":"fs","kind":"file","qualified_name":"src/shapes.rs","name":"shapes.rs","file_path":"src/shapes.rs","span":null,"metadata":{}},
   {"id":"imp","kind":"import","qualified_name":"src/lib.rs::crate::shapes::Circle","name":"crate::shapes::Circle","file_path":"src/lib.rs","span":[1,1,1,31],"metadata":{}},
   {"id":"c","kind":"class","qualified_name":"demo::shapes::Circle","name":"Circle","file_path":"src/shapes.rs","span":[1,1,1,19],"metadata":{"name_span":{"__span__":[1,12,1,18]}}}],
  "relations":[
   {"source_id":"imp","target_id":"c","kind":"resolves_to","metadata":{}},
   {"source_id":"imp","target_id":"c","kind":"resolves_to","metadata":{"span":{"__span__":[1,24,1,30]},"resolution":"name"}}]}"#;

#[test]
fn a_document_with_only_pub_use_is_exported() {
    let (bytes, warnings) = export(&Graph::from_json(PUB_USE_ONLY).unwrap());
    let index = decode_scip(&bytes);
    let lib = index
        .documents
        .iter()
        .find(|d| d.path == "src/lib.rs")
        .expect("src/lib.rs is a document");
    assert_eq!(lib.occurrences.len(), 1, "{:?}", lib.occurrences);
    let leaf = &lib.occurrences[0];
    assert_eq!(leaf.roles & 1, 0, "a reference, not a definition");
    assert!(leaf.symbol.contains("Circle"), "{}", leaf.symbol);
    assert_eq!((leaf.range[0], leaf.range[1]), (0, 23));
    // The structural RESOLVES_TO (no position) yields nothing and is not 'unplaced'.
    assert!(warnings.iter().all(|w| w.code != "no-reference-span"), "{warnings:?}");
}

#[test]
fn declaration_sites_and_includes_are_placed() {
    let (bytes, _) = export(&Graph::from_json(C_DECL_SITES).unwrap());
    let index = decode_scip(&bytes);
    let doc = |path: &str| index.documents.iter().find(|d| d.path == path).unwrap_or_else(|| panic!("no document {path}"));

    // The header has a document: the prototype is a forward definition, and the
    // file itself is defined on its first character because an include points at it.
    let header = doc("a.h");
    let proto = header.occurrences.iter().find(|o| o.symbol == "callix . c . . add().").expect("prototype");
    assert_eq!(proto.range, [2, 4, 7]);
    assert_eq!(proto.roles, 0x40, "ForwardDefinition");
    let file_def = header.occurrences.iter().find(|o| o.symbol == "callix . c . . `a.h`/").expect("file symbol");
    assert_eq!(file_def.range, [0, 0, 1]);
    assert_eq!(file_def.roles, 1, "Definition");

    // The include operands are plain references.
    let source = doc("a.c");
    let to_header = source.occurrences.iter().find(|o| o.symbol == "callix . c . . `a.h`/").expect("include of a.h");
    assert_eq!(to_header.range, [0, 9, 14]);
    assert_eq!(to_header.roles, 0);
    let to_stdio = source.occurrences.iter().find(|o| o.symbol.contains("stdio.h")).expect("include of stdio.h");
    assert_eq!(to_stdio.range, [1, 9, 18]);
    assert_eq!(to_stdio.roles, 0);

    // A bare-span declared_at entry is a position, not a declaration site.
    for d in &index.documents {
        assert!(d.occurrences.iter().all(|o| o.range != [8, 0, 4]), "{}: {:?}", d.path, d.occurrences);
        let mut sorted: Vec<_> = d.occurrences.iter().map(|o| o.range.clone()).collect();
        let before = sorted.clone();
        sorted.sort();
        assert_eq!(before, sorted, "{} is not sorted", d.path);
    }
}

#[test]
fn a_placed_module_is_a_definition_and_a_reference_target() {
    let (bytes, warnings) = export(&Graph::from_json(MODULE_GRAPH).unwrap());
    let index = decode_scip(&bytes);
    let util = index.documents.iter().find(|d| d.path == "pkg/util.py").unwrap();
    let defs: Vec<_> = util.occurrences.iter().filter(|o| o.roles & 1 == 1).collect();
    assert_eq!(defs.len(), 1, "{defs:?}");
    // The module's zero-width 1:1 span is defined on the first character.
    assert_eq!(defs[0].range, vec![0, 0, 1]);
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

const IMPORT_GRAPH: &str = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
  "nodes":[
   {"id":"p","kind":"project","qualified_name":"proj","name":"proj","file_path":null,"span":null,"metadata":{}},
   {"id":"fa","kind":"file","qualified_name":"app.py","name":"app.py","file_path":"app.py","span":null,"metadata":{}},
   {"id":"fl","kind":"file","qualified_name":"pkg/lib.py","name":"lib.py","file_path":"pkg/lib.py","span":null,"metadata":{}},
   {"id":"w","kind":"class","qualified_name":"pkg.lib.Widget","name":"Widget","file_path":"pkg/lib.py","span":[1,1,2,9],"metadata":{"name_span":{"__span__":[1,7,1,13]}}},
   {"id":"i","kind":"import","qualified_name":"app.Widget","name":"Widget","file_path":"app.py","span":null,"metadata":{}}],
  "relations":[
   {"source_id":"i","target_id":"w","kind":"resolves_to","metadata":{"span":{"__span__":[1,21,1,27]},"resolution":"definition"}},
   {"source_id":"i","target_id":"w","kind":"resolves_to","metadata":{}}]}"#;

#[test]
fn scip_exports_resolves_to_with_span() {
    let (bytes, _) = export(&Graph::from_json(IMPORT_GRAPH).unwrap());
    let index = decode_scip(&bytes);
    let app = index.documents.iter().find(|d| d.path == "app.py").expect("app.py document");
    let refs: Vec<_> = app.occurrences.iter().filter(|o| o.roles & 1 == 0).collect();
    assert_eq!(refs.len(), 1, "{refs:?}");
    assert_eq!(refs[0].range, [0, 20, 26]);
    assert!(refs[0].symbol.contains("pkg.lib.Widget"), "{}", refs[0].symbol);

    // Only the structural edge: nothing placed, nothing reported as unplaced.
    let bare = IMPORT_GRAPH.replace(
        r#"{"source_id":"i","target_id":"w","kind":"resolves_to","metadata":{"span":{"__span__":[1,21,1,27]},"resolution":"definition"}},"#,
        "",
    );
    let (bytes, warnings) = export(&Graph::from_json(&bare).unwrap());
    let index = decode_scip(&bytes);
    let app = index.documents.iter().find(|d| d.path == "app.py");
    assert!(app.is_none_or(|d| d.occurrences.iter().all(|o| o.roles & 1 != 0)));
    assert!(
        warnings.iter().all(|w| !w.code.contains("unplaced") && !w.code.contains("unsupported-reference")),
        "{warnings:?}"
    );
}

#[test]
fn scip_symbols_distinct_per_subroot() {
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"pl","kind":"project","qualified_name":"lib","name":"lib","file_path":null,"span":null,"metadata":{}},
       {"id":"ps","kind":"project","qualified_name":"src","name":"src","file_path":null,"span":null,"metadata":{}},
       {"id":"ml","kind":"module","qualified_name":"deps/lib","name":"deps/lib","file_path":null,"span":null,"metadata":{}},
       {"id":"ms","kind":"module","qualified_name":"src","name":"src","file_path":null,"span":null,"metadata":{}},
       {"id":"fl","kind":"file","qualified_name":"deps/lib/shim.h","name":"shim.h","file_path":"deps/lib/shim.h","span":null,"metadata":{}},
       {"id":"fs","kind":"file","qualified_name":"src/z.c","name":"z.c","file_path":"src/z.c","span":null,"metadata":{}},
       {"id":"zl","kind":"function","qualified_name":"zmalloc","name":"zmalloc","file_path":"deps/lib/shim.h","span":[1,1,1,36],"metadata":{"name_span":{"__span__":[1,7,1,14]}}},
       {"id":"zs","kind":"function","qualified_name":"zmalloc","name":"zmalloc","file_path":"src/z.c","span":[1,1,1,45],"metadata":{"name_span":{"__span__":[1,7,1,14]}}},
       {"id":"f","kind":"function","qualified_name":"f","name":"f","file_path":"src/z.c","span":[2,1,2,26],"metadata":{"name_span":{"__span__":[2,6,2,7]}}}],
      "relations":[
       {"source_id":"pl","target_id":"ml","kind":"contains","metadata":{}},
       {"source_id":"ml","target_id":"fl","kind":"contains","metadata":{}},
       {"source_id":"fl","target_id":"zl","kind":"declares","metadata":{}},
       {"source_id":"ps","target_id":"ms","kind":"contains","metadata":{}},
       {"source_id":"ms","target_id":"fs","kind":"contains","metadata":{}},
       {"source_id":"fs","target_id":"zs","kind":"declares","metadata":{}},
       {"source_id":"fs","target_id":"f","kind":"declares","metadata":{}},
       {"source_id":"f","target_id":"zs","kind":"calls","metadata":{"span":{"__span__":[2,12,2,19]}}}]}"#;
    let (bytes, _) = export(&Graph::from_json(json).unwrap());
    let index = decode_scip(&bytes);
    let defined_in = |symbol: &str| -> Vec<String> {
        index
            .documents
            .iter()
            .filter(|d| d.occurrences.iter().any(|o| o.symbol == symbol && o.roles & 1 == 1))
            .map(|d| d.path.clone())
            .collect()
    };
    assert_eq!(defined_in("callix . lib . . zmalloc()."), ["deps/lib/shim.h"]);
    assert_eq!(defined_in("callix . src . . zmalloc()."), ["src/z.c"]);
    let z = index.documents.iter().find(|d| d.path == "src/z.c").unwrap();
    let call = z.occurrences.iter().find(|o| o.roles & 1 == 0).expect("the call is a reference");
    assert_eq!(call.symbol, "callix . src . . zmalloc().");
    assert!(index.external.is_empty(), "{:?}", index.external);
}

#[test]
fn scip_single_project_symbols_unchanged() {
    // `a.f` sits under the project; `loose.g` is reached by no CONTAINS edge and
    // falls back to the first project. Both read exactly as before.
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"p","kind":"project","qualified_name":"proj","name":"proj","file_path":null,"span":null,"metadata":{}},
       {"id":"fa","kind":"file","qualified_name":"a.py","name":"a.py","file_path":"a.py","span":null,"metadata":{}},
       {"id":"f","kind":"function","qualified_name":"a.f","name":"f","file_path":"a.py","span":[1,1,2,10],"metadata":{"name_span":{"__span__":[1,5,1,6]}}},
       {"id":"g","kind":"function","qualified_name":"loose.g","name":"g","file_path":"a.py","span":[3,1,4,10],"metadata":{"name_span":{"__span__":[3,5,3,6]}}}],
      "relations":[
       {"source_id":"p","target_id":"fa","kind":"contains","metadata":{}},
       {"source_id":"fa","target_id":"f","kind":"declares","metadata":{}}]}"#;
    let (bytes, _) = export(&Graph::from_json(json).unwrap());
    let symbols: Vec<String> = decode_scip(&bytes).documents[0].occurrences.iter().map(|o| o.symbol.clone()).collect();
    assert_eq!(symbols, ["callix . proj . . `a.f`().", "callix . proj . . `loose.g`()."]);
}

#[test]
fn scip_class_and_type_alias_of_one_name_stay_distinct() {
    // `typedef struct Fp {..} Fp;` is two nodes; the alias is listed first.
    let json = r#"{"schema_version":1,"metadata":{"resolver_status":"ok"},
      "nodes":[
       {"id":"p","kind":"project","qualified_name":"p","name":"p","file_path":null,"span":null,"metadata":{}},
       {"id":"fa","kind":"file","qualified_name":"a.h","name":"a.h","file_path":"a.h","span":null,"metadata":{}},
       {"id":"ta","kind":"type_alias","qualified_name":"Fp","name":"Fp","file_path":"a.h","span":[1,1,3,5],"metadata":{"name_span":{"__span__":[3,3,3,5]}}},
       {"id":"c","kind":"class","qualified_name":"Fp","name":"Fp","file_path":"a.h","span":[1,9,3,2],"metadata":{"name_span":{"__span__":[1,16,1,18]}}}],
      "relations":[
       {"source_id":"p","target_id":"fa","kind":"contains","metadata":{}},
       {"source_id":"fa","target_id":"ta","kind":"declares","metadata":{}},
       {"source_id":"fa","target_id":"c","kind":"declares","metadata":{}}]}"#;
    let (bytes, _) = export(&Graph::from_json(json).unwrap());
    let occurrences = &decode_scip(&bytes).documents[0].occurrences;
    let count = |symbol: &str| occurrences.iter().filter(|o| o.symbol == symbol && o.roles & 1 == 1).count();
    assert_eq!(count("callix . p . . Fp#"), 1, "the class keeps the plain symbol");
    assert_eq!(count("callix . p . . type_alias/Fp#"), 1, "{occurrences:?}");
    assert_eq!(occurrences.len(), 2);
}
