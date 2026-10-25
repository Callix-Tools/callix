//! The command line, as clap sees it.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

const ABOUT: &str = "Polyglot code analysis: analyse a project once, then ask for the graph, \
an LSIF index, an SBOM or a report.";

const AFTER_HELP: &str = "\
Examples:
  callix analyze .                          write the graph (JSON) to stdout
  callix export . -f lsif -o dump.lsif      an LSIF index, e.g. for GitLab code navigation
  callix report . -t status                 how complete symbol resolution was
  callix export graph.json -f cyclonedx     export a graph saved earlier
  callix formats                            list every format, report and language
  callix doctor                             check the toolchains resolution relies on

Exit codes: 0 success, 1 failure, 2 usage error, 3 --strict refused the graph.";

#[derive(Parser)]
#[command(name = "callix", version, about = ABOUT, after_help = AFTER_HELP, propagate_version = true)]
pub struct Cli {
    /// Print nothing but the requested output and errors.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Analyse a project and write its graph as JSON.
    Analyze(AnalyzeArgs),
    /// Write the graph in another format (LSIF, CycloneDX, CSV, DOT, Mermaid).
    Export(ExportArgs),
    /// Print a report: summary, status, dependencies, boundaries or calls.
    Report(ReportArgs),
    /// Compare two saved graphs.
    Diff(DiffArgs),
    /// Merge saved graphs into one (cross-language analysis).
    Merge(MergeArgs),
    /// List the export formats, report types and languages.
    Formats,
    /// Check that the toolchains symbol resolution depends on are present.
    Doctor,
}

/// How to get the graph: analyse a directory, or read one saved earlier.
#[derive(Args)]
pub struct Source {
    /// A project directory to analyse, a graph JSON file, or `-` for stdin.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Languages to analyse (comma-separated); by default whatever is found.
    /// One of: python, typescript, go, rust, php, c, cpp, yaml.
    #[arg(short, long, value_delimiter = ',', value_name = "LANG")]
    pub lang: Vec<String>,

    /// Skip symbol resolution: a structural graph, fast, with no call edges.
    #[arg(long)]
    pub no_resolve: bool,

    /// Fail (exit 3) unless every language was fully resolved. PHP, C and C++
    /// are resolved by a symbol table and always count as degraded.
    #[arg(long)]
    pub strict: bool,

    /// Fetch the project's dependencies first (`go mod download`, `cargo fetch`,
    /// the JavaScript package manager) so that third-party symbols resolve.
    #[arg(long)]
    pub prepare: bool,

    /// Where the sources live, when INPUT is a saved graph. Lets exporters read
    /// the files to convert columns exactly.
    #[arg(long, value_name = "DIR")]
    pub source_root: Option<PathBuf>,
}

#[derive(Args)]
pub struct AnalyzeArgs {
    #[command(flatten)]
    pub source: Source,

    /// Write to FILE instead of stdout.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Indent the JSON (the default is one compact line).
    #[arg(long)]
    pub pretty: bool,
}

#[derive(Args)]
pub struct ExportArgs {
    #[command(flatten)]
    pub source: Source,

    /// Target format; see `callix formats`.
    #[arg(short, long, value_name = "FORMAT")]
    pub format: String,

    /// Write to FILE instead of stdout.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Draw only the edges touching this symbol (a node id or qualified name);
    /// used by `dot` and `mermaid`.
    #[arg(long, value_name = "SYMBOL")]
    pub symbol: Option<String>,

    /// The LSIF version to claim in the metadata.
    #[arg(long, value_name = "VERSION")]
    pub lsif_version: Option<String>,

    /// Exit with failure if the exporter had anything to warn about.
    #[arg(long)]
    pub deny_warnings: bool,
}

#[derive(Args)]
pub struct ReportArgs {
    #[command(flatten)]
    pub source: Source,

    /// The report: summary, status, dependencies, boundaries or calls.
    #[arg(short = 't', long = "type", value_name = "TYPE", default_value = "summary")]
    pub kind: String,

    /// Output style: text, md or json.
    #[arg(short, long, value_name = "STYLE", default_value = "text")]
    pub format: String,

    /// Write to FILE instead of stdout.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// For `calls`: the symbol (node id or qualified name) to show.
    #[arg(long, value_name = "SYMBOL")]
    pub symbol: Option<String>,
}

#[derive(Args)]
pub struct DiffArgs {
    /// The earlier graph (JSON).
    pub old: PathBuf,
    /// The later graph (JSON).
    pub new: PathBuf,
    /// Print the full difference as JSON.
    #[arg(long)]
    pub json: bool,
    /// Exit with status 1 when the graphs differ.
    #[arg(long)]
    pub exit_code: bool,
}

#[derive(Args)]
pub struct MergeArgs {
    /// Two or more graphs (JSON files).
    #[arg(required = true, num_args = 2..)]
    pub graphs: Vec<PathBuf>,
    /// Write to FILE instead of stdout.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Refuse to merge nodes that appear in more than one graph, instead of
    /// letting identical ones coincide.
    #[arg(long)]
    pub disjoint: bool,
    /// Indent the JSON.
    #[arg(long)]
    pub pretty: bool,
}
