//! End-to-end tests: the built binary against the repository's fixtures.
//!
//! These run the real adapters, so they need the same build the rest of the
//! suite does and nothing else — every fixture is analysed with a resolver that
//! ships inside the binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use callix_export::lsif::check::validate;
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn callix(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_callix")).args(args).output().expect("run callix")
}

fn ok(args: &[&str]) -> String {
    let out = callix(args);
    assert!(
        out.status.success(),
        "callix {args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("callix-cli-{}-{name}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn version_matches_the_workspace_manifest() {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml")).unwrap();
    let version = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.strip_suffix('"'))
        .unwrap();
    assert_eq!(ok(&["--version"]).trim(), format!("callix {version}"));
}

#[test]
fn formats_lists_everything() {
    let text = ok(&["formats"]);
    for needle in ["lsif", "cyclonedx", "csv-edges", "mermaid", "summary", "boundaries", "typescript"] {
        assert!(text.contains(needle), "{needle} missing from:\n{text}");
    }
}

#[test]
fn analyze_writes_the_graph_as_json() {
    let text = ok(&["analyze", p(&fixture("python")), "-q"]);
    let graph: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(graph["schema_version"], 1);
    assert_eq!(graph["metadata"]["resolver_status"], "ok");
    assert!(graph["nodes"].as_array().unwrap().len() > 30);
    // compact by default, one line
    assert_eq!(text.trim_end().lines().count(), 1);
}

#[test]
fn pretty_indents_the_graph() {
    let text = ok(&["analyze", p(&fixture("yaml")), "-q", "--pretty"]);
    assert!(text.lines().count() > 10);
}

#[test]
fn export_lsif_is_valid_and_points_at_the_source() {
    let dir = fixture("python");
    let text = ok(&["export", p(&dir), "-f", "lsif", "-q"]);
    let summary = validate(&text).unwrap_or_else(|p| panic!("{p:#?}"));
    assert!(summary.documents >= 2);
    assert!(summary.ranges > 20);
    let canonical = dir.canonicalize().unwrap();
    assert!(text.contains(&format!("file://{}", canonical.display())), "projectRoot missing");
    assert!(text.contains("\"name\":\"callix\""));
}

#[test]
fn export_to_a_file_and_from_a_saved_graph() {
    let dir = scratch("saved");
    let graph = dir.join("graph.json");
    ok(&["analyze", p(&fixture("go")), "-q", "-o", p(&graph)]);

    // A saved graph exports without analysing again, and with a source root
    // the LSIF columns can be converted exactly.
    let lsif = dir.join("dump.lsif");
    ok(&["export", p(&graph), "-f", "lsif", "-q", "--source-root", p(&fixture("go")), "-o", p(&lsif)]);
    let text = fs::read_to_string(&lsif).unwrap();
    validate(&text).unwrap_or_else(|p| panic!("{p:#?}"));
    assert!(text.contains("service.go"));

    let csv = ok(&["export", p(&graph), "-f", "csv-nodes", "-q"]);
    assert!(csv.starts_with("id,kind,qualified_name"));
    fs::remove_dir_all(dir).ok();
}

#[test]
fn a_graph_can_come_from_stdin() {
    use std::io::Write;
    use std::process::Stdio;

    let graph = ok(&["analyze", p(&fixture("typescript")), "-q"]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_callix"))
        .args(["report", "-", "-t", "summary", "-f", "json", "-q"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(graph.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["title"], "Graph summary");
}

#[test]
fn reports_in_every_style() {
    let dir = fixture("python");
    for style in ["text", "md", "json"] {
        for kind in ["summary", "status", "dependencies", "boundaries", "calls"] {
            let text = ok(&["report", p(&dir), "-t", kind, "-f", style, "-q"]);
            assert!(!text.trim().is_empty(), "{kind}/{style} printed nothing");
        }
    }
    let status = ok(&["report", p(&dir), "-t", "status", "-q"]);
    assert!(status.contains("every language in the graph was resolved by its backend"));
}

#[test]
fn calls_report_for_one_symbol() {
    let text = ok(&["report", p(&fixture("python")), "-t", "calls", "--symbol", "pkg.models.Engine.ping", "-q"]);
    assert!(text.contains("pkg.service.build"), "{text}");
}

#[test]
fn strict_refuses_a_degraded_language_with_exit_3() {
    let out = callix(&["analyze", p(&fixture("php")), "--strict", "-q"]);
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("degraded"), "{err}");
    assert!(out.stdout.is_empty(), "nothing is written when strict refuses");

    // and a fully resolved one passes
    assert!(callix(&["analyze", p(&fixture("python")), "--strict", "-q"]).status.success());
}

#[test]
fn no_resolve_gives_a_structural_graph() {
    let text = ok(&["analyze", p(&fixture("python")), "--no-resolve", "-q"]);
    let graph: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(graph["metadata"]["resolver_status"], "unavailable");
    let calls = graph["relations"].as_array().unwrap().iter().filter(|r| r["kind"] == "calls").count();
    assert_eq!(calls, 0);
}

#[test]
fn lang_selects_and_normalises() {
    let poly = scratch("poly");
    for lang in ["python", "go"] {
        let dest = poly.join(lang);
        fs::create_dir_all(&dest).unwrap();
        copy_dir(&fixture(lang), &dest);
    }
    let both: Value = serde_json::from_str(&ok(&["analyze", p(&poly), "-q"])).unwrap();
    let only: Value = serde_json::from_str(&ok(&["analyze", p(&poly), "-l", "golang", "-q"])).unwrap();
    assert!(both["nodes"].as_array().unwrap().len() > only["nodes"].as_array().unwrap().len());
    let names: Vec<_> = only["nodes"].as_array().unwrap().iter().map(|n| n["qualified_name"].as_str().unwrap()).collect();
    assert!(names.iter().all(|q| !q.starts_with("pkg.")), "python leaked into a go-only run");

    let out = callix(&["analyze", p(&poly), "-l", "cobol", "-q"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown language"));
    fs::remove_dir_all(poly).ok();
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&dest).unwrap();
            copy_dir(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[test]
fn bad_input_fails_with_a_message() {
    let empty = scratch("empty");
    let out = callix(&["analyze", p(&empty), "-q"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing to analyse"));

    let out = callix(&["export", p(&fixture("python")), "-f", "pdf", "-q"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown format") && err.contains("lsif"), "{err}");

    let out = callix(&["export", "/no/such/place", "-f", "lsif", "-q"]);
    assert_eq!(out.status.code(), Some(1));

    // usage errors are clap's: exit 2
    assert_eq!(callix(&["export"]).status.code(), Some(2));
    assert_eq!(callix(&["frobnicate"]).status.code(), Some(2));
    fs::remove_dir_all(empty).ok();
}

#[test]
fn merge_and_diff() {
    let dir = scratch("merge");
    let py = dir.join("py.json");
    let ts = dir.join("ts.json");
    ok(&["analyze", p(&fixture("python")), "-q", "-o", p(&py)]);
    ok(&["analyze", p(&fixture("typescript")), "-q", "-o", p(&ts)]);

    let merged = dir.join("merged.json");
    ok(&["merge", p(&py), p(&ts), "-q", "-o", p(&merged)]);
    let graph: Value = serde_json::from_str(&fs::read_to_string(&merged).unwrap()).unwrap();
    let a: Value = serde_json::from_str(&fs::read_to_string(&py).unwrap()).unwrap();
    let b: Value = serde_json::from_str(&fs::read_to_string(&ts).unwrap()).unwrap();
    let expected = a["nodes"].as_array().unwrap().len() + b["nodes"].as_array().unwrap().len();
    // the project node and the shared boundary coincide, so slightly fewer
    let got = graph["nodes"].as_array().unwrap().len();
    assert!(got <= expected && got + 4 >= expected, "{got} vs {expected}");

    assert_eq!(ok(&["diff", p(&py), p(&py)]).trim(), "no difference");
    let out = callix(&["diff", p(&py), p(&merged), "--exit-code"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("added nodes"));
    let json: Value = serde_json::from_str(&ok(&["diff", p(&py), p(&merged), "--json"])).unwrap();
    assert_eq!(json["is_empty"], false);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn doctor_reports_the_toolchains() {
    let text = ok(&["doctor"]);
    assert!(text.contains("no Python in this binary"));
    assert!(text.contains("rust-analyzer"));
    assert!(text.contains("PHP, C and C++"));
}

#[test]
fn cyclonedx_for_a_python_project() {
    let text = ok(&["export", p(&fixture("python")), "-f", "cyclonedx", "-q"]);
    let bom: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(bom["components"][0]["purl"], "pkg:pypi/httpx");
}

#[test]
fn prepare_runs_for_the_manifests_it_finds() {
    // A Go module with no requirements: `go mod download` succeeds offline.
    let dir = scratch("prepare");
    copy_dir(&fixture("go"), &dir);
    let out = callix(&["analyze", p(&dir), "--prepare", "-l", "go"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("preparing Go modules"), "{err}");
    fs::remove_dir_all(dir).ok();
}

#[test]
fn c_and_cpp_dumps_have_references() {
    // The C family resolves through a symbol table; its edges must still say
    // where they were written, or the dump has nothing to find.
    for lang in ["c", "cpp"] {
        let text = ok(&["export", p(&fixture(lang)), "-f", "lsif", "-q"]);
        validate(&text).unwrap_or_else(|p| panic!("{lang}: {p:#?}"));
        assert!(text.contains("\"property\":\"references\""), "{lang}: no references in the dump");
    }
}
