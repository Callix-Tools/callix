//! The subcommands.

use std::io::Write;
use std::process::{Command as Process, Stdio};

use callix::native::Language;
use callix_export::ops;
use callix_export::report::{self, Format, ReportKind};
use callix_export::{Options, Warning, exporter, exporters};
use serde_json::json;

use crate::cli::{AnalyzeArgs, Cli, Command, DiffArgs, ExportArgs, MergeArgs, ReportArgs};
use crate::failure::{Failure, Result};
use crate::input;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run(cli: &Cli) -> Result<()> {
    let quiet = cli.quiet;
    match &cli.command {
        Command::Analyze(args) => analyze(args, quiet),
        Command::Export(args) => export(args, quiet),
        Command::Report(args) => report_cmd(args, quiet),
        Command::Diff(args) => diff(args),
        Command::Merge(args) => merge(args, quiet),
        Command::Formats => formats(),
        Command::Doctor => doctor(),
    }
}

fn warn(warnings: &[Warning], quiet: bool) {
    if quiet {
        return;
    }
    for w in warnings {
        eprintln!("warning: [{}] {}", w.code, w.message);
    }
}

fn analyze(args: &AnalyzeArgs, quiet: bool) -> Result<()> {
    if !args.source.input.is_dir() {
        return Err(Failure::new(format!(
            "{}: `analyze` takes a project directory",
            args.source.input.display()
        )));
    }
    let (text, _) = input::analyze_dir(&args.source.input, &args.source, args.pretty, quiet)?;
    let mut out = input::output(args.output.as_deref())?;
    out.write_all(text.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn export(args: &ExportArgs, quiet: bool) -> Result<()> {
    // Fail on a bad format name before spending minutes analysing.
    let exporter = exporter(&args.format).map_err(|_| {
        Failure::new(format!(
            "unknown format {:?}; expected one of: {}",
            args.format,
            exporters().iter().map(|e| e.name()).collect::<Vec<_>>().join(", ")
        ))
    })?;
    let loaded = input::load(&args.source, quiet)?;
    let options = Options {
        source_root: loaded.source_root.clone(),
        tool_version: VERSION.to_owned(),
        lsif_version: args.lsif_version.clone(),
        symbol: args.symbol.clone(),
    };
    let mut out = input::output(args.output.as_deref())?;
    let warnings = exporter.export(&loaded.graph, &mut out, &options)?;
    out.flush()?;
    warn(&warnings, quiet);
    if args.deny_warnings && !warnings.is_empty() {
        return Err(Failure::new(format!("{} warning(s) with --deny-warnings", warnings.len())));
    }
    Ok(())
}

fn report_cmd(args: &ReportArgs, quiet: bool) -> Result<()> {
    let kind = ReportKind::parse(&args.kind).map_err(|_| {
        Failure::new(format!(
            "unknown report type {:?}; expected one of: {}",
            args.kind,
            ReportKind::ALL.iter().map(|k| k.name()).collect::<Vec<_>>().join(", ")
        ))
    })?;
    let style = Format::parse(&args.format)
        .map_err(|_| Failure::new(format!("unknown style {:?}; expected text, md or json", args.format)))?;
    let loaded = input::load(&args.source, quiet)?;
    let options = Options { symbol: args.symbol.clone(), ..Options::default() };
    let (report, warnings) = report::build(kind, &loaded.graph, &options);
    let mut out = input::output(args.output.as_deref())?;
    report.render(style, &mut out)?;
    out.flush()?;
    warn(&warnings, quiet);
    Ok(())
}

fn merge(args: &MergeArgs, quiet: bool) -> Result<()> {
    let mut graphs = args.graphs.iter();
    let first = graphs.next().expect("clap requires two graphs");
    let mut merged = input::read_graph(first)?;
    for path in graphs {
        let other = input::read_graph(path)?;
        merged
            .merge(&other, !args.disjoint)
            .map_err(|e| Failure::new(format!("{}: {e}", path.display())))?;
    }
    let mut out = input::output(args.output.as_deref())?;
    merged.write_json(&mut out, args.pretty.then_some(2))?;
    out.write_all(b"\n")?;
    out.flush()?;
    if !quiet {
        eprintln!(
            "merged {} graphs: {} nodes, {} relations",
            args.graphs.len(),
            merged.nodes.len(),
            merged.relations.len()
        );
    }
    Ok(())
}

fn diff(args: &DiffArgs) -> Result<()> {
    let (old, new) = (input::read_graph(&args.old)?, input::read_graph(&args.new)?);
    let delta = ops::diff(&old, &new);
    let different = !delta.is_empty();
    let node = |n: &callix_export::model::Node| {
        json!({"id": n.id, "kind": n.kind.as_str(), "qualified_name": n.qualified_name})
    };
    let relation = |r: &callix_export::model::Relation| {
        json!({"source_id": r.source_id, "target_id": r.target_id, "kind": r.kind.as_str()})
    };
    let info = json!({
        "is_empty": !different,
        "added_nodes": delta.added_nodes.iter().map(|n| node(n)).collect::<Vec<_>>(),
        "removed_nodes": delta.removed_nodes.iter().map(|n| node(n)).collect::<Vec<_>>(),
        "changed_nodes": delta.changed_nodes.iter().map(|(_, after)| node(after)).collect::<Vec<_>>(),
        "added_relations": delta.added_relations.iter().map(|r| relation(r)).collect::<Vec<_>>(),
        "removed_relations": delta.removed_relations.iter().map(|r| relation(r)).collect::<Vec<_>>(),
    });
    let mut out = input::output(None)?;
    if args.json {
        serde_json::to_writer_pretty(&mut out, &info)?;
        out.write_all(b"\n")?;
    } else if !different {
        writeln!(out, "no difference")?;
    } else {
        for (key, label) in [
            ("added_nodes", "added nodes"),
            ("removed_nodes", "removed nodes"),
            ("changed_nodes", "changed nodes"),
            ("added_relations", "added relations"),
            ("removed_relations", "removed relations"),
        ] {
            let items = info[key].as_array().map_or(&[][..], Vec::as_slice);
            if items.is_empty() {
                continue;
            }
            writeln!(out, "{label}: {}", items.len())?;
            for item in items.iter().take(20) {
                let line = if item.get("qualified_name").is_some() {
                    format!("  {} {}", item["kind"].as_str().unwrap_or(""), item["qualified_name"].as_str().unwrap_or(""))
                } else {
                    format!(
                        "  {} {} -> {}",
                        item["kind"].as_str().unwrap_or(""),
                        item["source_id"].as_str().unwrap_or(""),
                        item["target_id"].as_str().unwrap_or("")
                    )
                };
                writeln!(out, "{line}")?;
            }
            if items.len() > 20 {
                writeln!(out, "  ... and {} more", items.len() - 20)?;
            }
        }
    }
    out.flush()?;
    if args.exit_code && different {
        return Err(Failure::silent(1));
    }
    Ok(())
}

fn formats() -> Result<()> {
    let mut out = input::output(None)?;
    writeln!(out, "Export formats (callix export -f <name>):")?;
    for e in exporters() {
        writeln!(out, "  {:<10} .{:<9} {}", e.name(), e.extension(), e.description())?;
    }
    writeln!(out, "\nReport types (callix report -t <name>):")?;
    for k in ReportKind::ALL {
        writeln!(out, "  {:<13} {}", k.name(), k.description())?;
    }
    writeln!(out, "\nReport styles (callix report -f <style>): text, md, json")?;
    writeln!(out, "\nLanguages (--lang): {}", Language::ALL.map(Language::name).join(", "))?;
    out.flush()?;
    Ok(())
}

/// Runs `program args` and returns its first output line when it succeeds.
fn probe(program: &str, args: &[&str]) -> Option<String> {
    let output = Process::new(program).args(args).stderr(Stdio::null()).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().next().unwrap_or("").trim().to_owned())
}

fn doctor() -> Result<()> {
    let mut out = input::output(None)?;
    writeln!(out, "callix {VERSION} (graph schema 1; no Python in this binary)\n")?;

    // rust-analyzer on PATH is often a rustup proxy that exits with
    // "Unknown binary" when the component is not installed, so it has to run.
    let checks: [(&str, Option<String>, &str); 7] = [
        ("python", Some("built in".into()), "resolves Python (ty is linked in)"),
        ("typescript", Some("built in".into()), "resolves TypeScript (typescript-go is linked in)"),
        ("go", probe("go", &["version"]), "resolves Go (needs the toolchain and the module cache)"),
        ("cargo", probe("cargo", &["--version"]), "needed by rust-analyzer to load a workspace"),
        ("rust-analyzer", probe("rust-analyzer", &["--version"]), "resolves Rust"),
        ("npm", probe("npm", &["--version"]), "optional: --prepare installs JavaScript dependencies"),
        (
            "python3",
            probe("python3", &["--version"]),
            "optional: where a Python project's packages are installed, for ty to find",
        ),
    ];
    for (name, found, purpose) in checks {
        match found {
            Some(version) => writeln!(out, "  ok       {name:<14} {version}  — {purpose}")?,
            None => writeln!(out, "  missing  {name:<14} {purpose}")?,
        }
    }
    writeln!(
        out,
        "\nPHP, C and C++ are resolved by a symbol table and always report `degraded`; \
         YAML declares no symbols."
    )?;
    out.flush()?;
    Ok(())
}
