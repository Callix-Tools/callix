//! RFC 4180 tables: one row per node, or one row per relation.
//!
//! A graph is two tables, and a CSV file holds one, so there are two
//! exporters: `csv-nodes` and `csv-edges`. The edge table repeats the
//! endpoints' kind, name and file, which makes it usable on its own in a
//! spreadsheet or a `COPY`.

use std::io::Write;

use crate::error::Result;
use crate::model::Graph;
use crate::{Exporter, Options, Warning};

#[derive(Clone, Copy)]
enum Table {
    Nodes,
    Edges,
}

pub struct Csv {
    table: Table,
}

impl Csv {
    pub fn nodes() -> Self {
        Self { table: Table::Nodes }
    }

    pub fn edges() -> Self {
        Self { table: Table::Edges }
    }
}

/// Quotes a field only when it has to be.
fn field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn row(out: &mut dyn Write, fields: &[String]) -> Result<()> {
    let line = fields.iter().map(|f| field(f)).collect::<Vec<_>>().join(",");
    out.write_all(line.as_bytes())?;
    out.write_all(b"\r\n")?;
    Ok(())
}

fn num(value: Option<u32>) -> String {
    value.map_or_else(String::new, |v| v.to_string())
}

impl Exporter for Csv {
    fn name(&self) -> &'static str {
        match self.table {
            Table::Nodes => "csv-nodes",
            Table::Edges => "csv-edges",
        }
    }

    fn description(&self) -> &'static str {
        match self.table {
            Table::Nodes => "one CSV row per node",
            Table::Edges => "one CSV row per relation, with both endpoints spelled out",
        }
    }

    fn extension(&self) -> &'static str {
        "csv"
    }

    fn export(
        &self,
        graph: &Graph,
        out: &mut dyn Write,
        _options: &Options,
    ) -> Result<Vec<Warning>> {
        match self.table {
            Table::Nodes => {
                row(
                    out,
                    &["id", "kind", "qualified_name", "name", "file_path", "start_line", "start_col", "end_line", "end_col"]
                        .map(String::from),
                )?;
                for node in &graph.nodes {
                    let span = node.span;
                    row(
                        out,
                        &[
                            node.id.clone(),
                            node.kind.as_str().to_owned(),
                            node.qualified_name.clone(),
                            node.name.clone(),
                            node.file_path.clone().unwrap_or_default(),
                            num(span.map(|s| s.start_line)),
                            num(span.map(|s| s.start_col)),
                            num(span.map(|s| s.end_line)),
                            num(span.map(|s| s.end_col)),
                        ],
                    )?;
                }
            }
            Table::Edges => {
                row(
                    out,
                    &[
                        "kind", "source_id", "source_kind", "source_name", "source_file",
                        "target_id", "target_kind", "target_name", "target_file", "line", "col",
                    ]
                    .map(String::from),
                )?;
                for rel in &graph.relations {
                    let (source, target) = (graph.node(&rel.source_id), graph.node(&rel.target_id));
                    // Where the edge was written: a reference's span, or a
                    // boundary's line and column.
                    let (line, col) = match rel.span() {
                        Some(s) => (Some(s.start_line), Some(s.start_col)),
                        None => (
                            rel.metadata.get("line").and_then(|v| v.as_u64()).map(|v| v as u32),
                            rel.metadata.get("col").and_then(|v| v.as_u64()).map(|v| v as u32),
                        ),
                    };
                    row(
                        out,
                        &[
                            rel.kind.as_str().to_owned(),
                            rel.source_id.clone(),
                            source.map_or(String::new(), |n| n.kind.as_str().to_owned()),
                            source.map_or(String::new(), |n| n.qualified_name.clone()),
                            source.and_then(|n| n.file_path.clone()).unwrap_or_default(),
                            rel.target_id.clone(),
                            target.map_or(String::new(), |n| n.kind.as_str().to_owned()),
                            target.map_or(String::new(), |n| n.qualified_name.clone()),
                            target.and_then(|n| n.file_path.clone()).unwrap_or_default(),
                            num(line),
                            num(col),
                        ],
                    )?;
                }
            }
        }
        Ok(Vec::new())
    }
}
