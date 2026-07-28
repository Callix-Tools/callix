//! Узел графа: плоская модель с дискриминатором `kind` вместо иерархии
//! классов — так проще сериализовать и дешевле создавать в горячем цикле.

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
    /// Строковое значение — то же, что у `enum.Enum` в graphlens.
    #[getter]
    fn value(&self) -> &'static str {
        self.as_str()
    }

    /// Разбор из строкового значения; ValueError на неизвестном.
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
}
