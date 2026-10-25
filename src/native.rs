//! The engine behind the `callix` command line: every adapter, no Python.
//!
//! This module exists only in the build without the `python` feature. It is the
//! orchestration `driver.py` used to do inside an embedded interpreter — detect
//! the languages under a tree, run each adapter, merge the graphs, sum the
//! resolver counters — written once, in Rust, against the same adapters the
//! extension module exposes.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::cfamily::{CAdapter, CppAdapter};
use crate::golang::GoAdapter;
use crate::graph::Graph;
use crate::json::graph_to_json;
use crate::php::PhpAdapter;
use crate::py::Python;
use crate::python::PythonAdapter;
use crate::rustlang::RustAdapter;
use crate::typescript::TypeScriptAdapter;
use crate::yaml::YamlAdapter;

/// The languages with an adapter, in the order their graphs are merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Python,
    TypeScript,
    Go,
    Rust,
    Php,
    C,
    Cpp,
    Yaml,
}

impl Language {
    pub const ALL: [Self; 8] = [
        Self::Python,
        Self::TypeScript,
        Self::Go,
        Self::Rust,
        Self::Php,
        Self::C,
        Self::Cpp,
        Self::Yaml,
    ];

    /// The name `--lang` and the reports use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Go => "go",
            Self::Rust => "rust",
            Self::Php => "php",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Yaml => "yaml",
        }
    }

    /// Accepts the canonical names and the usual aliases.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim().to_ascii_lowercase().as_str() {
            "py" | "python" => Self::Python,
            "ts" | "tsx" | "js" | "javascript" | "typescript" => Self::TypeScript,
            "go" | "golang" => Self::Go,
            "rs" | "rust" => Self::Rust,
            "php" => Self::Php,
            "c" => Self::C,
            "cpp" | "c++" | "cxx" => Self::Cpp,
            "yml" | "yaml" => Self::Yaml,
            _ => return None,
        })
    }
}

/// An adapter of any language behind one interface. One lives at a time, so
/// the size gap between variants costs nothing.
#[allow(clippy::large_enum_variant)]
enum Adapter {
    Python(PythonAdapter),
    TypeScript(TypeScriptAdapter),
    Go(GoAdapter),
    Rust(RustAdapter),
    Php(PhpAdapter),
    C(CAdapter),
    Cpp(CppAdapter),
    Yaml(YamlAdapter),
}

impl Adapter {
    fn new(py: Python<'_>, language: Language, resolve: bool) -> Result<Self, String> {
        let built = match language {
            Language::Python => PythonAdapter::new(py, resolve, None, None, None).map(Self::Python),
            Language::TypeScript => TypeScriptAdapter::new(resolve, None, None, None).map(Self::TypeScript),
            Language::Go => GoAdapter::new(resolve, None, None, None).map(Self::Go),
            Language::Rust => RustAdapter::new(resolve, None, None, None).map(Self::Rust),
            Language::Php => PhpAdapter::new(resolve, None, None, None).map(Self::Php),
            Language::C => CAdapter::new(resolve, None, None, None).map(Self::C),
            Language::Cpp => CppAdapter::new(resolve, None, None, None).map(Self::Cpp),
            Language::Yaml => Ok(Self::Yaml(YamlAdapter::new(resolve, None))),
        };
        built.map_err(|e| e.to_string())
    }

    fn collect_files(&self, root: &Path) -> Vec<PathBuf> {
        let root = root.to_path_buf();
        match self {
            Self::Python(a) => a.collect_files(root),
            Self::TypeScript(a) => a.collect_files(root),
            Self::Go(a) => a.collect_files(root),
            Self::Rust(a) => a.collect_files(root),
            Self::Php(a) => a.collect_files(root),
            Self::C(a) => a.collect_files(root),
            Self::Cpp(a) => a.collect_files(root),
            Self::Yaml(a) => a.collect_files(root),
        }
    }

    fn analyze(&mut self, py: Python<'_>, root: &Path) -> Result<Graph, String> {
        let root = root.to_path_buf();
        match self {
            Self::Python(a) => a.analyze(py, root, None, false),
            Self::TypeScript(a) => a.analyze(py, root, None, false),
            Self::Go(a) => a.analyze(py, root, None, false),
            Self::Rust(a) => a.analyze(py, root, None, false),
            Self::Php(a) => a.analyze(py, root, None, false),
            Self::C(a) => a.analyze(py, root, None, false),
            Self::Cpp(a) => a.analyze(py, root, None, false),
            Self::Yaml(a) => a.analyze(py, root, None, false),
        }
        .map_err(|e| e.to_string())
    }
}

/// The languages whose adapter finds something to analyse under `root`.
///
/// Detection is by `collect_files`, not `can_handle`: the latter only looks at
/// the root itself, so a Go module in a subdirectory fails it while its sources
/// are found.
pub fn detect(root: &Path) -> Vec<Language> {
    Python::attach(|py| {
        Language::ALL
            .into_iter()
            .filter(|&language| {
                Adapter::new(py, language, false)
                    .is_ok_and(|adapter| !adapter.collect_files(root).is_empty())
            })
            .collect()
    })
}

/// What one adapter did.
#[derive(Debug, Clone)]
pub struct LanguageReport {
    pub language: Language,
    /// `ok`, `degraded` or `unavailable`; none when the adapter failed.
    pub status: Option<String>,
    pub nodes: usize,
    pub relations: usize,
    pub seconds: f64,
    pub error: Option<String>,
}

/// The merged graph, as JSON, and how each language fared.
#[derive(Debug)]
pub struct Analysis {
    /// Empty when no adapter produced a graph.
    pub json: String,
    pub languages: Vec<LanguageReport>,
    pub empty: bool,
}

/// Runs `languages` (or whatever is detected) over `root` and merges the graphs.
///
/// A language that fails is reported and skipped; the caller decides whether
/// that is an error. Graphs merge with `allow_shared`, so a boundary seen from
/// both sides becomes one node.
pub fn analyze_tree(
    root: &Path,
    languages: &[Language],
    resolve: bool,
    indent: Option<u32>,
) -> Analysis {
    let languages: Vec<Language> =
        if languages.is_empty() { detect(root) } else { languages.to_vec() };

    Python::attach(|py| {
        let mut merged: Option<Graph> = None;
        let mut reports = Vec::new();
        let mut counters: Vec<[f64; 6]> = Vec::new();

        for language in languages {
            let started = Instant::now();
            let mut report = LanguageReport {
                language,
                status: None,
                nodes: 0,
                relations: 0,
                seconds: 0.0,
                error: None,
            };
            let outcome = Adapter::new(py, language, resolve)
                .and_then(|mut adapter| adapter.analyze(py, root));
            match outcome {
                Err(error) => report.error = Some(error),
                Ok(graph) => {
                    report.status = graph.status_text(py);
                    report.nodes = graph.node_count();
                    report.relations = graph.relation_count();
                    counters.extend(graph.metric_counters(py));
                    match merged.as_mut() {
                        None => merged = Some(graph),
                        Some(base) => {
                            if let Err(error) = base.merge(py, &graph, true) {
                                report.error = Some(error.to_string());
                            }
                        }
                    }
                }
            }
            report.seconds = (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
            reports.push(report);
        }

        let empty = merged.is_none();
        let json = merged.map_or_else(String::new, |graph| {
            // One language keeps its own counters untouched; several are summed.
            if counters.len() > 1 {
                graph.set_summed_metrics(py, &counters);
            }
            graph_to_json(py, &graph, indent)
        });
        Analysis { json, languages: reports, empty }
    })
}
