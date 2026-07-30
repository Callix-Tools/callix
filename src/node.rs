//! A graph node: a flat model with a `kind` discriminator instead of a class
//! hierarchy — easier to serialize and cheaper to build in a hot loop.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pyclass_enum, gen_stub_pymethods};

use crate::span::Span;

#[gen_stub_pyclass_enum]
#[pyclass(module = "callix._core", eq, eq_int, frozen, hash, from_py_object)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum NodeKind {
    #[pyo3(name = "PROJECT")]
    Project,
    #[pyo3(name = "MODULE")]
    Module,
    #[pyo3(name = "FILE")]
    File,
    #[pyo3(name = "CLASS")]
    Class,
    #[pyo3(name = "FUNCTION")]
    Function,
    #[pyo3(name = "METHOD")]
    Method,
    #[pyo3(name = "PARAMETER")]
    Parameter,
    #[pyo3(name = "IMPORT")]
    Import,
    #[pyo3(name = "DEPENDENCY")]
    Dependency,
    #[pyo3(name = "EXTERNAL_SYMBOL")]
    ExternalSymbol,
    #[pyo3(name = "VARIABLE")]
    Variable,
    #[pyo3(name = "ATTRIBUTE")]
    Attribute,
    #[pyo3(name = "TYPE_ALIAS")]
    TypeAlias,
    #[pyo3(name = "BOUNDARY")]
    Boundary,
}

impl NodeKind {
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
        Some(match value {
            "project" => Self::Project,
            "module" => Self::Module,
            "file" => Self::File,
            "class" => Self::Class,
            "function" => Self::Function,
            "method" => Self::Method,
            "parameter" => Self::Parameter,
            "import" => Self::Import,
            "dependency" => Self::Dependency,
            "external_symbol" => Self::ExternalSymbol,
            "variable" => Self::Variable,
            "attribute" => Self::Attribute,
            "type_alias" => Self::TypeAlias,
            "boundary" => Self::Boundary,
            _ => return None,
        })
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl NodeKind {
    /// The string value — the same one `enum.Enum` carries in graphlens.
    #[getter]
    fn value(&self) -> &'static str {
        self.as_str()
    }

    /// Parses from a string value; raises ValueError on an unknown one.
    #[staticmethod]
    fn from_value(value: &str) -> PyResult<Self> {
        Self::parse(value).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!("unknown NodeKind: {value:?}"))
        })
    }

    fn __str__(&self) -> String {
        format!("NodeKind.{}", self.as_str().to_uppercase())
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    /// The graph contract is 14 node kinds. `as_str` and `parse` are two
    /// independent match arms over the same enum: a kind added to one and
    /// forgotten in the other is invisible from Python, because nothing there
    /// enumerates the Rust variants.
    const ALL: [NodeKind; 14] = [
        NodeKind::Project,
        NodeKind::Module,
        NodeKind::File,
        NodeKind::Class,
        NodeKind::Function,
        NodeKind::Method,
        NodeKind::Parameter,
        NodeKind::Import,
        NodeKind::Dependency,
        NodeKind::ExternalSymbol,
        NodeKind::Variable,
        NodeKind::Attribute,
        NodeKind::TypeAlias,
        NodeKind::Boundary,
    ];

    /// A second, independent spelling of the table — the wire values are part
    /// of the on-disk format, so they are pinned rather than derived. The
    /// match is exhaustive: a new variant fails to compile until it is added
    /// here and to `ALL`.
    fn expected_str(kind: NodeKind) -> &'static str {
        match kind {
            NodeKind::Project => "project",
            NodeKind::Module => "module",
            NodeKind::File => "file",
            NodeKind::Class => "class",
            NodeKind::Function => "function",
            NodeKind::Method => "method",
            NodeKind::Parameter => "parameter",
            NodeKind::Import => "import",
            NodeKind::Dependency => "dependency",
            NodeKind::ExternalSymbol => "external_symbol",
            NodeKind::Variable => "variable",
            NodeKind::Attribute => "attribute",
            NodeKind::TypeAlias => "type_alias",
            NodeKind::Boundary => "boundary",
        }
    }

    #[test]
    fn there_are_exactly_fourteen_node_kinds() {
        assert_eq!(ALL.len(), 14);
    }

    #[test]
    fn as_str_and_parse_round_trip_for_every_kind() {
        for kind in ALL {
            assert_eq!(kind.as_str(), expected_str(kind));
            assert_eq!(
                NodeKind::parse(kind.as_str()),
                Some(kind),
                "{} does not round-trip",
                kind.as_str()
            );
        }
    }

    #[test]
    fn every_wire_value_is_distinct() {
        for (i, left) in ALL.iter().enumerate() {
            for right in &ALL[i + 1..] {
                assert_ne!(left.as_str(), right.as_str());
                assert_ne!(left, right);
            }
        }
    }

    #[test]
    fn parse_rejects_an_unknown_value() {
        // Upper case is the Python enum's member name, not its value.
        for value in ["", "FUNCTION", "typealias", "external symbol", "unknown"] {
            assert_eq!(NodeKind::parse(value), None, "{value:?} was accepted");
        }
    }
}

#[gen_stub_pyclass]
#[pyclass(module = "callix._core", frozen, get_all)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub qualified_name: String,
    pub name: String,
    pub file_path: Option<String>,
    pub span: Option<Span>,
    pub metadata: Py<PyDict>,
}

#[gen_stub_pymethods]
#[pymethods]
impl Node {
    #[new]
    #[pyo3(signature = (id, kind, qualified_name, name, file_path = None, span = None, metadata = None))]
    // The signature is the node's public API: every field is set on creation.
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        id: String,
        kind: NodeKind,
        qualified_name: String,
        name: String,
        file_path: Option<String>,
        span: Option<Span>,
        metadata: Option<Py<PyDict>>,
    ) -> Self {
        Self {
            id,
            kind,
            qualified_name,
            name,
            file_path,
            span,
            metadata: metadata.unwrap_or_else(|| PyDict::new(py).unbind()),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "Node(id={:?}, kind={}, qualified_name={:?})",
            self.id,
            self.kind.as_str(),
            self.qualified_name
        )
    }

    pub fn __eq__(&self, py: Python<'_>, other: &Self) -> PyResult<bool> {
        Ok(self.id == other.id
            && self.kind == other.kind
            && self.qualified_name == other.qualified_name
            && self.name == other.name
            && self.file_path == other.file_path
            && self.span == other.span
            && self.metadata.bind(py).eq(other.metadata.bind(py))?)
    }

    /// Hashes the id alone.
    ///
    /// Defining `__eq__` in Python removes the inherited `__hash__`, which
    /// left nodes unhashable — `set(graph.nodes.values())` raised a TypeError
    /// on a graph library. A content hash is not available because `__eq__`
    /// compares a metadata dict, but the id is enough: equality requires the
    /// ids to match, so equal nodes always hash equal, which is the direction
    /// the contract demands. Two nodes sharing an id and differing in metadata
    /// collide, and colliding is allowed.
    fn __hash__(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.id.hash(&mut hasher);
        hasher.finish()
    }
}
