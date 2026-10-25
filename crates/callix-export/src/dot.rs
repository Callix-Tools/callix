//! Graphviz DOT and Mermaid, for looking at a graph rather than loading it.
//!
//! The picture is the *behavioural* graph — who calls, extends or uses whom —
//! because a drawing of every `CONTAINS` and `DECLARES` edge is a hairball.
//! Pass a symbol (`Options::symbol`) to draw only the edges touching it.

use std::collections::HashSet;
use std::io::Write;

use crate::error::Result;
use crate::model::{Graph, Node, RelationKind};
use crate::{Exporter, Options, Warning};

/// Mermaid stops rendering past a few hundred edges (its default
/// `maxEdges` is 500).
const MERMAID_EDGE_LIMIT: usize = 500;

pub struct Dot {
    pub mermaid: bool,
}

fn drawn(kind: RelationKind) -> bool {
    matches!(
        kind,
        RelationKind::Calls
            | RelationKind::InheritsFrom
            | RelationKind::HasType
            | RelationKind::References
            | RelationKind::CommunicatesWith
    )
}

fn matches_symbol(node: &Node, symbol: &str) -> bool {
    node.id == symbol || node.qualified_name == symbol
}

impl Exporter for Dot {
    fn name(&self) -> &'static str {
        if self.mermaid { "mermaid" } else { "dot" }
    }

    fn description(&self) -> &'static str {
        if self.mermaid {
            "Mermaid flowchart of calls, inheritance and type use"
        } else {
            "Graphviz DOT of calls, inheritance and type use"
        }
    }

    fn extension(&self) -> &'static str {
        if self.mermaid { "mmd" } else { "dot" }
    }

    fn export(
        &self,
        graph: &Graph,
        out: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Warning>> {
        let mut warnings = Vec::new();

        let mut edges: Vec<(&str, &str, RelationKind)> = Vec::new();
        let mut seen: HashSet<(&str, &str, RelationKind)> = HashSet::new();
        for rel in &graph.relations {
            if !drawn(rel.kind) {
                continue;
            }
            if let Some(symbol) = &options.symbol {
                let touches = [&rel.source_id, &rel.target_id]
                    .iter()
                    .any(|id| graph.node(id).is_some_and(|n| matches_symbol(n, symbol)));
                if !touches {
                    continue;
                }
            }
            let key = (rel.source_id.as_str(), rel.target_id.as_str(), rel.kind);
            if seen.insert(key) {
                edges.push(key);
            }
        }
        if edges.is_empty() {
            warnings.push(Warning::new(
                "empty-graph",
                "no calls, inheritance or type-use edges to draw (resolution off, or the symbol \
                 matches nothing)",
            ));
        }

        // Nodes in graph order, only those an edge touches.
        let used: HashSet<&str> = edges.iter().flat_map(|(s, t, _)| [*s, *t]).collect();
        let nodes: Vec<&Node> = graph.nodes.iter().filter(|n| used.contains(n.id.as_str())).collect();

        if self.mermaid {
            if edges.len() > MERMAID_EDGE_LIMIT {
                warnings.push(Warning::new(
                    "mermaid-too-large",
                    format!(
                        "{} edges exceed Mermaid's default limit of {MERMAID_EDGE_LIMIT}; \
                         narrow it with --symbol or use dot",
                        edges.len()
                    ),
                ));
            }
            writeln!(out, "flowchart LR")?;
            for node in &nodes {
                writeln!(out, "  {}[\"{}\"]", node.id_label(), escape_mermaid(&node.qualified_name))?;
            }
            for (source, target, kind) in &edges {
                let arrow = if *kind == RelationKind::Calls {
                    "-->".to_owned()
                } else {
                    format!("-- {} -->", kind.as_str())
                };
                writeln!(out, "  n{source} {arrow} n{target}")?;
            }
        } else {
            writeln!(out, "digraph callix {{")?;
            writeln!(out, "  rankdir=LR;")?;
            writeln!(out, "  node [shape=box, fontname=\"Helvetica\"];")?;
            for node in &nodes {
                writeln!(
                    out,
                    "  \"{}\" [label=\"{}\\n({})\"];",
                    node.id,
                    escape_dot(&node.qualified_name),
                    node.kind.as_str()
                )?;
            }
            for (source, target, kind) in &edges {
                if *kind == RelationKind::Calls {
                    writeln!(out, "  \"{source}\" -> \"{target}\";")?;
                } else {
                    writeln!(out, "  \"{source}\" -> \"{target}\" [label=\"{}\"];", kind.as_str())?;
                }
            }
            writeln!(out, "}}")?;
        }
        Ok(warnings)
    }
}

trait IdLabel {
    fn id_label(&self) -> String;
}

impl IdLabel for Node {
    fn id_label(&self) -> String {
        format!("n{}", self.id)
    }
}

fn escape_dot(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_mermaid(text: &str) -> String {
    text.replace('"', "#quot;")
}
