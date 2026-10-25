mod common;

use callix_export::model::Graph;
use callix_export::report::{self, Format, ReportKind};
use callix_export::{Options, exporter, exporters};
use common::SAMPLE;

fn graph() -> Graph {
    Graph::from_json(SAMPLE).unwrap()
}

fn run(name: &str, options: &Options) -> (String, Vec<callix_export::Warning>) {
    let mut out = Vec::new();
    let warnings = exporter(name).unwrap().export(&graph(), &mut out, options).unwrap();
    (String::from_utf8(out).unwrap(), warnings)
}

#[test]
fn registry_lists_every_format_once() {
    let names: Vec<_> = exporters().iter().map(|e| e.name()).collect();
    assert_eq!(names, ["lsif", "cyclonedx", "csv-nodes", "csv-edges", "dot", "mermaid"]);
    assert!(exporter("nope").is_err());
}

#[test]
fn csv_nodes_has_a_header_and_a_row_per_node() {
    let (text, _) = run("csv-nodes", &Options::default());
    let lines: Vec<_> = text.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1 + 10);
    assert!(lines[0].starts_with("id,kind,qualified_name"));
    assert!(lines.iter().any(|l| l.starts_with("f,function,a.charge,charge,a.py,1,1,2,12")));
}

#[test]
fn csv_quotes_what_needs_quoting() {
    let json = r#"{"schema_version":1,"metadata":{},"nodes":[
      {"id":"a","kind":"function","qualified_name":"x, \"y\"","name":"n","file_path":null,"span":null,"metadata":{}}],
      "relations":[]}"#;
    let mut out = Vec::new();
    exporter("csv-nodes").unwrap().export(&Graph::from_json(json).unwrap(), &mut out, &Options::default()).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("\"x, \"\"y\"\"\""), "{text}");
}

#[test]
fn csv_edges_spell_out_both_endpoints_and_the_position() {
    let (text, _) = run("csv-edges", &Options::default());
    assert!(text.contains("calls,g,function,b.checkout,b.py,e1,external_symbol,httpx.get,,5,5"));
    // boundary edges position themselves with line/col
    assert!(text.contains("consumes,g,function,b.checkout,b.py,bd,boundary,http:GET /pay/{},,5,5"));
}

#[test]
fn dot_draws_calls_only_by_default() {
    let (text, warnings) = run("dot", &Options::default());
    assert!(text.starts_with("digraph callix {"));
    assert!(text.contains("\"g\" -> \"f\";"));
    assert!(text.contains("\"g\" -> \"x\" [label=\"references\"];"));
    assert!(!text.contains("depends_on"));
    assert!(warnings.is_empty());
}

#[test]
fn dot_and_mermaid_can_focus_on_one_symbol() {
    let opts = Options { symbol: Some("b.refund".into()), ..Options::default() };
    let (dot, _) = run("dot", &opts);
    assert!(dot.contains("\"h\" -> \"f\";"));
    assert!(!dot.contains("\"g\""));

    let (mermaid, _) = run("mermaid", &opts);
    assert!(mermaid.starts_with("flowchart LR"));
    assert!(mermaid.contains("nh --> nf"));
}

#[test]
fn an_empty_picture_is_a_warning() {
    let opts = Options { symbol: Some("no.such".into()), ..Options::default() };
    let (_, warnings) = run("dot", &opts);
    assert_eq!(warnings[0].code, "empty-graph");
}

#[test]
fn cyclonedx_lists_the_declared_dependencies() {
    let (text, warnings) = run("cyclonedx", &Options { tool_version: "1.2.3".into(), ..Options::default() });
    let bom: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(bom["bomFormat"], "CycloneDX");
    assert_eq!(bom["specVersion"], "1.5");
    assert_eq!(bom["metadata"]["component"]["name"], "shop");
    assert_eq!(bom["metadata"]["tools"]["components"][0]["version"], "1.2.3");
    // the sample's files are all python, so the ecosystem is known
    assert_eq!(bom["components"][0]["purl"], "pkg:pypi/httpx");
    assert_eq!(bom["dependencies"][0]["dependsOn"][0], "pkg:pypi/httpx");
    assert!(warnings.iter().any(|w| w.code == "no-versions"));
    assert!(!warnings.iter().any(|w| w.code == "unknown-ecosystem"));
}

fn render(kind: ReportKind, style: Format, symbol: Option<&str>) -> String {
    let options = Options { symbol: symbol.map(str::to_owned), ..Options::default() };
    let (report, _) = report::build(kind, &graph(), &options);
    let mut out = Vec::new();
    report.render(style, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn summary_counts_nodes_and_relations() {
    let text = render(ReportKind::Summary, Format::Text, None);
    let value = |key: &str| {
        text.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().last())
            .map(str::to_owned)
    };
    assert_eq!(value("nodes").as_deref(), Some("10"), "{text}");
    assert_eq!(value("relations").as_deref(), Some("9"));
    assert!(text.contains("python (2)"));
    assert!(text.contains("function"));
}

#[test]
fn status_explains_the_resolver_state() {
    let text = render(ReportKind::Status, Format::Markdown, None);
    assert!(text.contains("- **status**: ok"));
    assert!(text.contains("100.0%"));
    assert!(text.contains("| python | 2 | ty (embedded) | ok |"));
}

#[test]
fn dependencies_and_boundaries_reports() {
    let deps = render(ReportKind::Dependencies, Format::Json, None);
    let json: serde_json::Value = serde_json::from_str(&deps).unwrap();
    assert_eq!(json["sections"][1]["rows"][0]["package"], "httpx");
    assert_eq!(json["sections"][1]["rows"][0]["declared by"], "shop");

    let bounds = render(ReportKind::Boundaries, Format::Json, None);
    let json: serde_json::Value = serde_json::from_str(&bounds).unwrap();
    let row = &json["sections"][1]["rows"][0];
    assert_eq!(row["key"], "GET /pay/{}");
    assert_eq!(row["exposed by"], "a.charge");
    assert_eq!(row["consumed by"], "b.checkout");
}

#[test]
fn calls_report_for_a_symbol_lists_callers_and_callees() {
    let text = render(ReportKind::Calls, Format::Text, Some("a.charge"));
    assert!(text.contains("b.checkout (function)"));
    assert!(text.contains("b.refund (function)"));
    let text = render(ReportKind::Calls, Format::Text, Some("b.checkout"));
    assert!(text.contains("httpx.get (external_symbol)"));
}

#[test]
fn unknown_names_are_errors() {
    assert!(ReportKind::parse("nope").is_err());
    assert!(Format::parse("pdf").is_err());
    assert_eq!(Format::parse("md").unwrap(), Format::Markdown);
}

#[test]
fn a_foreign_schema_version_is_rejected() {
    let err = Graph::from_json(r#"{"schema_version":2,"metadata":{},"nodes":[],"relations":[]}"#).unwrap_err();
    assert!(err.to_string().contains("schema_version"));
    assert!(Graph::from_json("not json").is_err());
    let unknown = r#"{"schema_version":1,"metadata":{},"nodes":[{"id":"a","kind":"gadget","qualified_name":"a","name":"a"}],"relations":[]}"#;
    assert!(Graph::from_json(unknown).unwrap_err().to_string().contains("unknown NodeKind"));
}
