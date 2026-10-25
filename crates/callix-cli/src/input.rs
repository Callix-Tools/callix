//! Getting a graph: analyse a directory, or read one saved earlier.

use std::cell::OnceCell;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use callix_export::Graph;

use crate::bridge::{AnalyzeInfo, Bridge};
use crate::cli::Source;
use crate::failure::{Failure, Result};
use crate::prepare;

/// The languages the adapters cover, as `--lang` spells them.
pub const LANGUAGES: [&str; 8] = ["python", "typescript", "go", "rust", "php", "c", "cpp", "yaml"];

/// The embedded interpreter, started the first time something needs it: a
/// saved graph can be exported without ever loading Python.
#[derive(Default)]
pub struct Engine {
    cell: OnceCell<Bridge>,
}

impl Engine {
    pub fn get(&self) -> Result<&Bridge> {
        if self.cell.get().is_none() {
            let bridge = Bridge::start()?;
            let _ = self.cell.set(bridge);
        }
        Ok(self.cell.get().expect("just set"))
    }
}

pub struct Loaded {
    pub graph: Graph,
    /// The directory the graph's file paths are relative to, when known.
    pub source_root: Option<PathBuf>,
}

/// `--lang` values, spelled however the user likes, to the canonical names.
pub fn normalize_languages(raw: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for item in raw {
        let name = match item.trim().to_ascii_lowercase().as_str() {
            "py" | "python" => "python",
            "ts" | "tsx" | "js" | "javascript" | "typescript" => "typescript",
            "go" | "golang" => "go",
            "rs" | "rust" => "rust",
            "php" => "php",
            "c" => "c",
            "cpp" | "c++" | "cxx" => "cpp",
            "yml" | "yaml" => "yaml",
            other => {
                return Err(Failure::new(format!(
                    "unknown language {other:?}; expected one of: {}",
                    LANGUAGES.join(", ")
                )));
            }
        };
        if !out.iter().any(|l| l == name) {
            out.push(name.to_owned());
        }
    }
    Ok(out)
}

fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// Analyses a directory and returns the graph JSON as the driver wrote it.
pub fn analyze_dir(
    engine: &Engine,
    dir: &Path,
    source: &Source,
    pretty: bool,
    quiet: bool,
) -> Result<(String, PathBuf)> {
    let languages = normalize_languages(&source.lang)?;
    let root = dir
        .canonicalize()
        .map_err(|e| Failure::new(format!("{}: {e}", dir.display())))?;

    if source.prepare {
        for problem in prepare::run(&root, quiet) {
            if !quiet {
                eprintln!("warning: [prepare] {problem}");
            }
        }
    }

    let started = std::time::Instant::now();
    let bridge = engine.get()?;
    let (text, info) = bridge.analyze(&root, &languages, !source.no_resolve, pretty)?;
    report_analysis(&root, &languages, &info, started.elapsed().as_secs_f64(), source, quiet)?;
    Ok((text, root))
}

/// Prints the per-language summary and applies the failure rules:
/// an explicitly requested language that failed is an error, one found by
/// detection only a warning, and `--strict` refuses anything not `ok`.
fn report_analysis(
    root: &Path,
    requested: &[String],
    info: &AnalyzeInfo,
    seconds: f64,
    source: &Source,
    quiet: bool,
) -> Result<()> {
    if info.empty {
        let tried = if requested.is_empty() { LANGUAGES.join(", ") } else { requested.join(", ") };
        let details: Vec<String> = info
            .languages
            .iter()
            .filter_map(|l| l.error.as_ref().map(|e| format!("{}: {e}", l.language)))
            .collect();
        let mut message = format!("nothing to analyse under {} (looked for: {tried})", root.display());
        if !details.is_empty() {
            message = format!("analysis failed under {}:\n  {}", root.display(), details.join("\n  "));
        }
        return Err(Failure::new(message));
    }

    for entry in &info.languages {
        if let Some(error) = &entry.error {
            let line = format!("{}: {error}", entry.language);
            if requested.contains(&entry.language) {
                return Err(Failure::new(line));
            }
            if !quiet {
                eprintln!("warning: [analysis] {line}");
            }
        }
    }

    if !quiet {
        eprintln!("analysed {} in {seconds:.1}s", root.display());
        for entry in info.languages.iter().filter(|e| e.error.is_none()) {
            eprintln!(
                "  {:<11} {:<12} {:>10} nodes {:>10} relations {:>7.1}s",
                entry.language,
                entry.status.as_deref().unwrap_or("-"),
                count(entry.nodes),
                count(entry.relations),
                entry.seconds
            );
        }
    }

    if source.strict {
        let weak: Vec<String> = info
            .languages
            .iter()
            .filter(|e| e.error.is_none() && e.status.as_deref() != Some("ok"))
            .map(|e| format!("{} ({})", e.language, e.status.as_deref().unwrap_or("unknown")))
            .collect();
        if !weak.is_empty() {
            return Err(Failure::strict(format!(
                "--strict: not fully resolved: {}",
                weak.join(", ")
            )));
        }
    }
    Ok(())
}

fn read_input(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut text = String::new();
        io::stdin().read_to_string(&mut text)?;
        Ok(text)
    } else {
        fs::read_to_string(path).map_err(|e| Failure::new(format!("{}: {e}", path.display())))
    }
}

/// Loads the graph for `export` and `report`.
pub fn load(engine: &Engine, source: &Source, quiet: bool) -> Result<Loaded> {
    let input = &source.input;
    if input != Path::new("-") && input.is_dir() {
        let (text, root) = analyze_dir(engine, input, source, false, quiet)?;
        return Ok(Loaded { graph: Graph::from_json(&text)?, source_root: Some(root) });
    }

    if input != Path::new("-") && !input.exists() {
        return Err(Failure::new(format!("{}: no such file or directory", input.display())));
    }
    if (!source.lang.is_empty() || source.prepare || source.no_resolve) && !quiet {
        eprintln!("warning: --lang, --prepare and --no-resolve apply to a directory; ignored for a saved graph");
    }
    let graph = Graph::from_json(&read_input(input)?)
        .map_err(|e| Failure::new(format!("{}: {e}", input.display())))?;
    if source.strict && graph.resolver_status() != Some("ok") {
        return Err(Failure::strict(format!(
            "--strict: the saved graph's resolver status is {:?}",
            graph.resolver_status().unwrap_or("unknown")
        )));
    }
    Ok(Loaded { graph, source_root: source.source_root.clone() })
}

/// stdout, or a buffered file.
pub fn output(path: Option<&Path>) -> Result<Box<dyn Write>> {
    Ok(match path {
        Some(path) => Box::new(io::BufWriter::new(
            fs::File::create(path).map_err(|e| Failure::new(format!("{}: {e}", path.display())))?,
        )),
        None => Box::new(io::BufWriter::new(io::stdout().lock())),
    })
}
