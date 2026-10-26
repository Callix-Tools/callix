//! The canonical graph JSON, read without Python.
//!
//! This is the same document `Graph.to_json()` writes (`SCHEMA_VERSION` 1).
//! Metadata stays an open JSON object: the exporters read the few keys they
//! understand and ignore the rest, so a graph written by a newer adapter still
//! exports.

use std::collections::HashMap;
use std::io::Read;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// The schema this build reads. Equality, not a range — as in
/// `ensure_schema_version`.
pub const SCHEMA_VERSION: u32 = 1;

/// Tag a span carries inside metadata: `{"__span__": [l, c, l, c]}`.
pub const SPAN_TAG: &str = "__span__";

pub type Metadata = Map<String, Value>;

/// A 1-based, end-exclusive source range. Columns count bytes of the line, as
/// tree-sitter reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl Span {
    /// Reads `[l, c, l, c]` or the tagged form `{"__span__": [l, c, l, c]}`.
    pub fn from_value(value: &Value) -> Option<Self> {
        let list = match value {
            Value::Array(list) => list,
            Value::Object(map) => map.get(SPAN_TAG)?.as_array()?,
            _ => return None,
        };
        if list.len() != 4 {
            return None;
        }
        let mut nums = [0u32; 4];
        for (slot, item) in nums.iter_mut().zip(list) {
            *slot = u32::try_from(item.as_u64()?).ok()?;
        }
        Some(Self {
            start_line: nums[0],
            start_col: nums[1],
            end_line: nums[2],
            end_col: nums[3],
        })
    }
}

/// The 14 node kinds, by their wire value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Project,
    Module,
    File,
    Class,
    Function,
    Method,
    Parameter,
    Import,
    Dependency,
    ExternalSymbol,
    Variable,
    Attribute,
    TypeAlias,
    Boundary,
}

impl NodeKind {
    pub const ALL: [Self; 14] = [
        Self::Project,
        Self::Module,
        Self::File,
        Self::Class,
        Self::Function,
        Self::Method,
        Self::Parameter,
        Self::Import,
        Self::Dependency,
        Self::ExternalSymbol,
        Self::Variable,
        Self::Attribute,
        Self::TypeAlias,
        Self::Boundary,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Module => "module",
            Self::File => "file",
            Self::Class => "class",
            Self::Function => "function",
            Self::Method => "method",
            Self::Parameter => "parameter",
            Self::Import => "import",
            Self::Dependency => "dependency",
            Self::ExternalSymbol => "external_symbol",
            Self::Variable => "variable",
            Self::Attribute => "attribute",
            Self::TypeAlias => "type_alias",
            Self::Boundary => "boundary",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// The 12 relation kinds, by their wire value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RelationKind {
    Contains,
    Declares,
    Imports,
    Calls,
    References,
    DependsOn,
    ResolvesTo,
    InheritsFrom,
    HasType,
    Exposes,
    Consumes,
    CommunicatesWith,
}

impl RelationKind {
    pub const ALL: [Self; 12] = [
        Self::Contains,
        Self::Declares,
        Self::Imports,
        Self::Calls,
        Self::References,
        Self::DependsOn,
        Self::ResolvesTo,
        Self::InheritsFrom,
        Self::HasType,
        Self::Exposes,
        Self::Consumes,
        Self::CommunicatesWith,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Declares => "declares",
            Self::Imports => "imports",
            Self::Calls => "calls",
            Self::References => "references",
            Self::DependsOn => "depends_on",
            Self::ResolvesTo => "resolves_to",
            Self::InheritsFrom => "inherits_from",
            Self::HasType => "has_type",
            Self::Exposes => "exposes",
            Self::Consumes => "consumes",
            Self::CommunicatesWith => "communicates_with",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// The kinds a resolver produces from a use-site: the edges whose
    /// `metadata["span"]` says where the reference was written.
    pub fn is_reference(self) -> bool {
        matches!(
            self,
            Self::Calls | Self::References | Self::HasType | Self::InheritsFrom
        )
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub qualified_name: String,
    pub name: String,
    pub file_path: Option<String>,
    pub span: Option<Span>,
    pub metadata: Metadata,
}

impl Node {
    /// The span of the identifier alone (`metadata["name_span"]`), which is
    /// what an index wants — `span` is the whole extent of the definition.
    pub fn name_span(&self) -> Option<Span> {
        Span::from_value(self.metadata.get("name_span")?)
    }

    pub fn meta_str(&self, key: &str) -> Option<&str> {
        self.metadata.get(key)?.as_str()
    }
}

#[derive(Clone, Debug)]
pub struct Relation {
    pub source_id: String,
    pub target_id: String,
    pub kind: RelationKind,
    pub metadata: Metadata,
}

impl Relation {
    /// Where the edge was written (`metadata["span"]`), when it records that.
    pub fn span(&self) -> Option<Span> {
        Span::from_value(self.metadata.get("span")?)
    }

    /// The file the edge was written in (`metadata["file"]`), recorded only
    /// when it is not the file of the edge's source node — see
    /// `Graph.note_site_file` in the engine for when that happens.
    pub fn site_file(&self) -> Option<&str> {
        self.metadata.get("file")?.as_str()
    }
}

#[derive(Deserialize)]
struct RawNode {
    id: String,
    kind: String,
    qualified_name: String,
    name: String,
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    span: Option<Value>,
    #[serde(default)]
    metadata: Metadata,
}

#[derive(Deserialize)]
struct RawRelation {
    source_id: String,
    target_id: String,
    kind: String,
    #[serde(default)]
    metadata: Metadata,
}

#[derive(Deserialize)]
struct RawGraph {
    schema_version: u64,
    #[serde(default)]
    metadata: Metadata,
    nodes: Vec<RawNode>,
    relations: Vec<RawRelation>,
}

/// A graph as written by `Graph.to_json()`.
#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub metadata: Metadata,
    pub nodes: Vec<Node>,
    pub relations: Vec<Relation>,
    index: HashMap<String, usize>,
}

impl Graph {
    pub fn from_json(text: &str) -> Result<Self> {
        Self::from_raw(serde_json::from_str(text)?)
    }

    pub fn from_reader(reader: impl Read) -> Result<Self> {
        Self::from_raw(serde_json::from_reader(reader)?)
    }

    pub fn from_value(value: Value) -> Result<Self> {
        Self::from_raw(serde_json::from_value(value)?)
    }

    fn from_raw(raw: RawGraph) -> Result<Self> {
        if raw.schema_version != u64::from(SCHEMA_VERSION) {
            return Err(Error::Schema {
                found: raw.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        let mut graph = Self { metadata: raw.metadata, ..Self::default() };
        for node in raw.nodes {
            let kind = NodeKind::parse(&node.kind)
                .ok_or_else(|| Error::Invalid(format!("unknown NodeKind: {:?}", node.kind)))?;
            let span = match &node.span {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    Span::from_value(value)
                        .ok_or_else(|| Error::Invalid(format!("bad span on node {}", node.id)))?,
                ),
            };
            graph.push_node(Node {
                id: node.id,
                kind,
                qualified_name: node.qualified_name,
                name: node.name,
                file_path: node.file_path,
                span,
                metadata: node.metadata,
            })?;
        }
        for relation in raw.relations {
            let kind = RelationKind::parse(&relation.kind).ok_or_else(|| {
                Error::Invalid(format!("unknown RelationKind: {:?}", relation.kind))
            })?;
            graph.relations.push(Relation {
                source_id: relation.source_id,
                target_id: relation.target_id,
                kind,
                metadata: relation.metadata,
            });
        }
        Ok(graph)
    }

    /// Adds a node whose id the caller has already checked.
    pub(crate) fn push_node_unchecked(&mut self, node: Node) {
        self.index.insert(node.id.clone(), self.nodes.len());
        self.nodes.push(node);
    }

    fn push_node(&mut self, node: Node) -> Result<()> {
        if self.index.contains_key(&node.id) {
            return Err(Error::Invalid(format!("duplicate node id {}", node.id)));
        }
        self.index.insert(node.id.clone(), self.nodes.len());
        self.nodes.push(node);
        Ok(())
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.index.get(id).map(|&i| &self.nodes[i])
    }

    pub fn nodes_of(&self, kind: NodeKind) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(move |n| n.kind == kind)
    }

    pub fn relations_of(&self, kind: RelationKind) -> impl Iterator<Item = &Relation> {
        self.relations.iter().filter(move |r| r.kind == kind)
    }

    /// `metadata["resolver_status"]`: `ok`, `degraded` or `unavailable`.
    pub fn resolver_status(&self) -> Option<&str> {
        self.metadata.get("resolver_status")?.as_str()
    }
}
