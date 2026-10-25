//! The subcommands.

use std::io::Write;
use std::process::{Command as Process, Stdio};

use callix_export::report::{self, Format, ReportKind};
use callix_export::{Options, Warning, exporter, exporters};
use serde_json::Value;

use crate::cli::{AnalyzeArgs, Cli, Command, DiffArgs, ExportArgs, MergeArgs, ReportArgs};
use crate::failure::{Failure, Result};
use crate::input::{self, Engine, LANGUAGES};

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run(cli: &Cli) -> Result<()> {
    let engine = Engine::default();
    let quiet = cli.quiet;
    match &cli.command {
        Command::Analyze(args) => analyze(&engine, args, quiet),
        Command::Export(args) => export(&engine, args, quiet),
        Command::Report(args) => report_cmd(&engine, args, quiet),
        Command::Diff(args) => diff(&engine, args),
        Command::Merge(args) => merge(&engine, args, quiet),
        Command::Formats => formats(),
        Command::Doctor => doctor(&engine),
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

fn analyze(engine: &Engine, args: &AnalyzeArgs, quiet: bool) -> Result<()> {
    if !args.source.input.is_dir() {
        return Err(Failure::new(format!(
            "{}: `analyze` takes a project directory",
            args.source.input.display()
        )));
    }
    let (text, _) = input::analyze_dir(engine, &args.source.input, &args.source, args.pretty, quiet)?;
    let mut out = input::output(args.output.as_deref())?;
    out.write_all(text.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn export(engine: &Engine, args: &ExportArgs, quiet: bool) -> Result<()> {
    // Fail on a bad format name before spending minutes analysing.
    let exporter = exporter(&args.format).map_err(|_| {
        Failure::new(format!(
            "unknown format {:?}; expected one of: {}",
            args.format,
            exporters().iter().map(|e| e.name()).collect::<Vec<_>>().join(", ")
        ))
    })?;
    let loaded = input::load(engine, &args.source, quiet)?;
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

fn report_cmd(engine: &Engine, args: &ReportArgs, quiet: bool) -> Result<()> {
    let kind = ReportKind::parse(&args.kind).map_err(|_| {
        Failure::new(format!(
            "unknown report type {:?}; expected one of: {}",
            args.kind,
            ReportKind::ALL.iter().map(|k| k.name()).collect::<Vec<_>>().join(", ")
        ))
    })?;
    let style = Format::parse(&args.format)
        .map_err(|_| Failure::new(format!("unknown style {:?}; expected text, md or json", args.format)))?;
    let loaded = input::load(engine, &args.source, quiet)?;
    let options = Options { symbol: args.symbol.clone(), ..Options::default() };
    let (report, warnings) = report::build(kind, &loaded.graph, &options);
    let mut out = input::output(args.output.as_deref())?;
    report.render(style, &mut out)?;
    out.flush()?;
    warn(&warnings, quiet);
    Ok(())
}

fn merge(engine: &Engine, args: &MergeArgs, _quiet: bool) -> Result<()> {
    let (text, info) = engine.get()?.merge(&args.graphs, !args.disjoint, args.pretty)?;
    let mut out = input::output(args.output.as_deref())?;
    out.write_all(text.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    if !_quiet {
        eprintln!(
            "merged {} graphs: {} nodes, {} relations",
            info["merged"], info["nodes"], info["relations"]
        );
    }
    Ok(())
}

fn diff(engine: &Engine, args: &DiffArgs) -> Result<()> {
    let info = engine.get()?.diff(&args.old, &args.new)?;
    let different = info["is_empty"] != Value::Bool(true);
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
    writeln!(out, "\nLanguages (--lang): {}", LANGUAGES.join(", "))?;
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

fn doctor(engine: &Engine) -> Result<()> {
    let mut out = input::output(None)?;
    let env = engine.get()?.environment()?;
    writeln!(
        out,
        "callix {VERSION} (engine {}, graph schema {})",
        env["callix"].as_str().unwrap_or("?"),
        env["schema_version"]
    )?;
    writeln!(
        out,
        "embedded Python {} (prefix {})\n",
        env["python"].as_str().unwrap_or("?"),
        env["base_prefix"].as_str().unwrap_or("?")
    )?;

    // rust-analyzer on PATH is often a rustup proxy that exits with
    // "Unknown binary" when the component is not installed, so it has to run.
    let checks: [(&str, Option<String>, &str); 6] = [
        ("python", Some(format!("embedded {}", env["python"].as_str().unwrap_or("?"))), "resolves Python (ty is linked in)"),
        ("typescript", Some("embedded".into()), "resolves TypeScript (typescript-go is linked in)"),
        ("go", probe("go", &["version"]), "resolves Go (needs the toolchain and the module cache)"),
        ("cargo", probe("cargo", &["--version"]), "needed by rust-analyzer to load a workspace"),
        ("rust-analyzer", probe("rust-analyzer", &["--version"]), "resolves Rust"),
        ("npm", probe("npm", &["--version"]), "optional: --prepare installs JavaScript dependencies"),
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

