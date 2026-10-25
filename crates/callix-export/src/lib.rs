//! Exporters for the callix graph.
//!
//! The input is the canonical graph JSON (`Graph.to_json()`, schema version 1),
//! read into [`model::Graph`] without any Python. Every exporter is a pure
//! function of that graph: it writes to a [`std::io::Write`] as it goes, so a
//! graph of a million nodes never exists a second time as one string, and it
//! returns the [`Warning`]s about what the target format could not carry.

pub mod csv;
pub mod cyclonedx;
pub mod dot;
pub mod error;
pub mod lsif;
pub mod model;
pub mod report;

use std::io::Write;
use std::path::PathBuf;

pub use error::{Error, Result};
pub use model::Graph;

/// Something an exporter could not express, or a reason to doubt the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// A short stable slug, for filtering.
    pub code: &'static str,
    pub message: String,
}

impl Warning {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

/// Knobs shared by all exporters; each reads only what it understands.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The directory the graph's relative `file_path`s are relative to. LSIF
    /// needs it for `projectRoot` and to turn byte columns into UTF-16 ones.
    pub source_root: Option<PathBuf>,
    /// Recorded as the producing tool's version (`toolInfo`).
    pub tool_version: String,
    /// The LSIF version to claim. Defaults to [`lsif::DEFAULT_VERSION`].
    pub lsif_version: Option<String>,
    /// Restrict a report to one symbol: a node id or a qualified name.
    pub symbol: Option<String>,
}

pub trait Exporter {
    /// The name a user passes as `--format`.
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// The conventional file extension, without the dot.
    fn extension(&self) -> &'static str;
    fn export(
        &self,
        graph: &Graph,
        out: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Warning>>;
}

/// Every exporter, in the order `--help` lists them.
pub fn exporters() -> Vec<Box<dyn Exporter>> {
    vec![
        Box::new(lsif::Lsif),
        Box::new(cyclonedx::CycloneDx),
        Box::new(csv::Csv::nodes()),
        Box::new(csv::Csv::edges()),
        Box::new(dot::Dot { mermaid: false }),
        Box::new(dot::Dot { mermaid: true }),
    ]
}

pub fn exporter(name: &str) -> Result<Box<dyn Exporter>> {
    exporters()
        .into_iter()
        .find(|e| e.name() == name)
        .ok_or_else(|| Error::UnknownFormat(name.to_owned()))
}
