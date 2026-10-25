//! A CycloneDX 1.5 SBOM from the graph's DEPENDENCY nodes.
//!
//! What the graph knows is the *declared* dependencies: a name per package, as
//! the manifests list it. It records neither a version nor an ecosystem, so the
//! ecosystem is taken from the project's source files when they are all one
//! language, and the components carry no version. Both gaps are reported.

use std::collections::BTreeSet;
use std::io::Write;

use serde_json::json;

use crate::error::Result;
use crate::lsif::language_id;
use crate::model::{Graph, NodeKind, RelationKind};
use crate::{Exporter, Options, Warning};

pub struct CycloneDx;

/// The purl type for the package manager of a language, when it has one.
fn purl_type(language: &str) -> Option<&'static str> {
    Some(match language {
        "python" => "pypi",
        "typescript" | "javascript" => "npm",
        "go" => "golang",
        "rust" => "cargo",
        "php" => "composer",
        _ => return None,
    })
}

/// A package URL for a name; npm scopes (`@a/b`) become the purl namespace.
fn purl(kind: &str, name: &str) -> String {
    let encoded = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    match kind {
        "npm" if name.starts_with('@') => {
            let (scope, rest) = name.split_once('/').unwrap_or((name, ""));
            format!("pkg:npm/{}/{}", encoded(scope), encoded(rest))
        }
        // purl wants the namespace separated by a literal '/'.
        "golang" | "composer" | "github" => {
            let parts: Vec<String> = name.split('/').map(&encoded).collect();
            format!("pkg:{kind}/{}", parts.join("/"))
        }
        _ => format!("pkg:{kind}/{}", encoded(name)),
    }
}

impl Exporter for CycloneDx {
    fn name(&self) -> &'static str {
        "cyclonedx"
    }

    fn description(&self) -> &'static str {
        "CycloneDX 1.5 SBOM of the declared dependencies (JSON)"
    }

    fn extension(&self) -> &'static str {
        "cdx.json"
    }

    fn export(
        &self,
        graph: &Graph,
        out: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Warning>> {
        let mut warnings = Vec::new();

        let languages: BTreeSet<&str> = graph
            .nodes_of(NodeKind::File)
            .filter_map(|n| n.file_path.as_deref())
            .map(language_id)
            .filter(|l| !matches!(*l, "plaintext" | "yaml"))
            .collect();
        let ecosystem = match languages.iter().collect::<Vec<_>>().as_slice() {
            [only] => purl_type(only),
            _ => None,
        };
        if ecosystem.is_none() {
            warnings.push(Warning::new(
                "unknown-ecosystem",
                "the sources are not a single language with a package manager, so components \
                 are given generic package URLs",
            ));
        }
        let kind = ecosystem.unwrap_or("generic");

        let project = graph.nodes_of(NodeKind::Project).next();
        let project_name = project.map_or("unknown", |p| p.name.as_str());
        let project_ref = format!("project:{project_name}");

        let mut components = Vec::new();
        let mut depends_on = Vec::new();
        for rel in graph.relations_of(RelationKind::DependsOn) {
            let Some(dep) = graph.node(&rel.target_id).filter(|n| n.kind == NodeKind::Dependency)
            else {
                continue;
            };
            let reference = purl(kind, &dep.name);
            components.push(json!({
                "type": "library",
                "bom-ref": reference,
                "name": dep.name,
                "purl": reference,
            }));
            depends_on.push(reference);
        }
        if components.is_empty() {
            warnings.push(Warning::new("no-dependencies", "the graph declares no dependencies"));
        } else {
            warnings.push(Warning::new(
                "no-versions",
                "the graph records declared names only; components carry no version",
            ));
        }

        let bom = json!({
            "bomFormat": "CycloneDX",
            "specVersion": "1.5",
            "version": 1,
            "metadata": {
                "tools": {"components": [
                    {"type": "application", "name": "callix", "version": options.tool_version}
                ]},
                "component": {"type": "application", "bom-ref": project_ref, "name": project_name},
            },
            "components": components,
            "dependencies": [{"ref": project_ref, "dependsOn": depends_on}],
        });
        serde_json::to_writer_pretty(&mut *out, &bom)?;
        out.write_all(b"\n")?;
        Ok(warnings)
    }
}
