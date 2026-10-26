//! Getting a graph: analyse a directory, or read one saved earlier.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use callix::native::{self, Analysis, Language};
use callix_export::Graph;

use crate::cli::Source;
use crate::failure::{Failure, Result};
use crate::prepare;

pub struct Loaded {
    pub graph: Graph,
    /// The directory the graph's file paths are relative to, when known.
    pub source_root: Option<PathBuf>,
}

/// `--lang` values, spelled however the user likes, to the languages.
pub fn parse_languages(raw: &[String]) -> Result<Vec<Language>> {
    let mut out: Vec<Language> = Vec::new();
    for item in raw {
        let language = Language::parse(item).ok_or_else(|| {
            Failure::new(format!(
                "unknown language {:?}; expected one of: {}",
                item.trim(),
                Language::ALL.map(Language::name).join(", ")
            ))
        })?;
        if !out.contains(&language) {
            out.push(language);
        }
    }
    Ok(out)
}

fn count(n: usize) -> String {
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

/// Analyses a directory and returns the graph JSON as the engine wrote it.
pub fn analyze_dir(
    dir: &Path,
    source: &Source,
    pretty: bool,
    quiet: bool,
) -> Result<(String, PathBuf)> {
    let languages = parse_languages(&source.lang)?;
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
    let analysis = native::analyze_tree(&root, &languages, !source.no_resolve, pretty.then_some(2));
    report_analysis(&root, &languages, &analysis, started.elapsed().as_secs_f64(), source, quiet)?;
    Ok((analysis.json, root))
}

/// Prints the per-language summary and applies the failure rules:
/// an explicitly requested language that failed is an error, one found by
/// detection only a warning, and `--strict` refuses anything not `ok`.
fn report_analysis(
    root: &Path,
    requested: &[Language],
    analysis: &Analysis,
    seconds: f64,
    source: &Source,
    quiet: bool,
) -> Result<()> {
    if analysis.empty {
        let tried = if requested.is_empty() {
            Language::ALL.map(Language::name).join(", ")
        } else {
            requested.iter().map(|l| l.name()).collect::<Vec<_>>().join(", ")
        };
        let details: Vec<String> = analysis
            .languages
            .iter()
            .filter_map(|l| l.error.as_ref().map(|e| format!("{}: {e}", l.language.name())))
            .collect();
        let message = if details.is_empty() {
            format!("nothing to analyse under {} (looked for: {tried})", root.display())
        } else {
            format!("analysis failed under {}:\n  {}", root.display(), details.join("\n  "))
        };
        return Err(Failure::new(message));
    }

    for entry in &analysis.languages {
        if let Some(error) = &entry.error {
            let line = format!("{}: {error}", entry.language.name());
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
        for entry in analysis.languages.iter().filter(|e| e.error.is_none()) {
            eprintln!(
                "  {:<11} {:<12} {:>10} nodes {:>10} relations {:>7.1}s",
                entry.language.name(),
                entry.status.as_deref().unwrap_or("-"),
                count(entry.nodes),
                count(entry.relations),
                entry.seconds
            );
            let calls = &entry.calls;
            if calls.total > 0 {
                // What "ok" does not say: where the calls actually landed.
                let mut parts = vec![format!("{} to a definition", count(calls.definition + calls.flow))];
                if calls.flow > 0 {
                    parts.push(format!("of which {} followed through a value", count(calls.flow)));
                }
                if calls.name > 0 {
                    parts.push(format!("{} matched by name", count(calls.name)));
                    if calls.ambiguous > 0 {
                        parts.push(format!("{} of them ambiguous", count(calls.ambiguous)));
                    }
                }
                if calls.value > 0 {
                    parts.push(format!("{} stopped at a value", count(calls.value)));
                }
                parts.push(format!("{} outside the project", count(calls.external)));
                if calls.unlocated > 0 {
                    parts.push(format!("{} not bound", count(calls.unlocated)));
                }
                eprintln!("  {:<11} {} calls: {}", "", count(calls.total), parts.join(", "));
            }
        }
    }

    if source.strict {
        let weak: Vec<String> = analysis
            .languages
            .iter()
            .filter(|e| e.error.is_none() && e.status.as_deref() != Some("ok"))
            .map(|e| format!("{} ({})", e.language.name(), e.status.as_deref().unwrap_or("unknown")))
            .collect();
        if !weak.is_empty() {
            return Err(Failure::strict(format!("--strict: not fully resolved: {}", weak.join(", "))));
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

/// Reads a saved graph (a file, or `-` for stdin).
pub fn read_graph(path: &Path) -> Result<Graph> {
    Graph::from_json(&read_input(path)?).map_err(|e| Failure::new(format!("{}: {e}", path.display())))
}

/// Loads the graph for `export` and `report`.
pub fn load(source: &Source, quiet: bool) -> Result<Loaded> {
    let input = &source.input;
    if input != Path::new("-") && input.is_dir() {
        let (text, root) = analyze_dir(input, source, false, quiet)?;
        return Ok(Loaded { graph: Graph::from_json(&text)?, source_root: Some(root) });
    }

    if input != Path::new("-") && !input.exists() {
        return Err(Failure::new(format!("{}: no such file or directory", input.display())));
    }
    if (!source.lang.is_empty() || source.prepare || source.no_resolve) && !quiet {
        eprintln!("warning: --lang, --prepare and --no-resolve apply to a directory; ignored for a saved graph");
    }
    let graph = read_graph(input)?;
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
