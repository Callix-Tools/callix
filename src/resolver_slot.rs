//! Where an adapter's symbol definitions come from.
//!
//! Every language adapter takes the same two constructor arguments — `resolve`
//! and `resolver` — and those two collapse into the three states below. Sharing
//! the enum is not just deduplication: it is what makes `resolver=` mean the
//! same thing in seven adapters instead of only in `PythonAdapter`, which is
//! where it started and where it stayed for too long.
//!
//! A custom resolver **replaces** the built-in backend; it is not consulted
//! after it. The protocol is the same three methods for every language, and it
//! is deliberately positional:
//!
//! ```text
//! prepare(project_root: Path, files: list[Path]) -> None
//! resolve_all(queries: list[tuple[Path, int, int]]) -> list[ResolvedRef | None]
//! status() -> ResolverStatus | str
//! ```
//!
//! A query is `(file, line, col)` — 1-based, pointing at a use-site — and the
//! answer is where that name is *defined*. Answers are matched to queries by
//! index, so a resolver must return one item per query and use `None` for "I do
//! not know" rather than dropping it. `resolve_all` gets every occurrence in one
//! call because the built-in backends all amortize a workspace load, and a
//! per-site protocol would have made that impossible to write.
//!
//! [`ResolverSlot`] is generic over the native backend and carries no bound, so
//! an adapter whose own resolver is not position-keyed can still use the enum
//! and the custom-protocol helpers; the C family does exactly that, matching on
//! [`ResolverSlot::Native`] itself. The methods that ask the backend a question
//! live in the `N: NativeResolver` impl.

use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3::types::PyList;

use crate::occurrence::OccurrenceRef;
use crate::python::ResolvedRef;
use crate::status::ResolverStatus;

/// A backend compiled into callix, driven directly rather than through Python.
///
/// The three methods are the native mirror of the Python protocol above. The
/// `prepare` signature is the union of what the five backends need — Python,
/// TypeScript, Go, Rust and PHP: ty and rust-analyzer index a whole root and
/// ignore `files`, while the PHP symbol table is built from exactly the files
/// the walk collected.
pub(crate) trait NativeResolver {
    fn prepare(&mut self, project_root: &Path, files: &[PathBuf]) -> PyResult<()>;

    fn resolve(&self, path: &str, line: u32, col: u32) -> Option<ResolvedRef>;

    fn status(&self) -> ResolverStatus;
}

/// The resolution backend an adapter was constructed with.
pub(crate) enum ResolverSlot<N> {
    /// The backend that ships with callix for this language.
    Native(N),
    /// A Python object with `prepare` / `resolve_all` / `status`.
    Custom(Py<PyAny>),
    /// `resolve=False`: the graph stays structural.
    Disabled,
}

impl<N> ResolverSlot<N> {
    /// Builds the slot from the two arguments every adapter's `__new__` takes.
    ///
    /// `resolve=False` wins over a custom resolver, because that combination is
    /// almost always a caller turning resolution off to time the structural
    /// phase, and silently resolving anyway would be the worse reading.
    pub(crate) fn new(
        resolve: bool,
        custom: Option<Py<PyAny>>,
        native: impl FnOnce() -> PyResult<N>,
    ) -> PyResult<Self> {
        Ok(match (resolve, custom) {
            (false, _) => Self::Disabled,
            (true, Some(object)) => Self::Custom(object),
            (true, None) => Self::Native(native()?),
        })
    }

    pub(crate) fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// The `__repr__` name for this slot; `native` names the built-in backend.
    pub(crate) fn label(&self, native: &'static str) -> &'static str {
        match self {
            Self::Native(_) => native,
            Self::Custom(_) => "custom",
            Self::Disabled => "off",
        }
    }
}

impl<N: NativeResolver> ResolverSlot<N> {
    /// Loads the workspace, once per `analyze()` call.
    pub(crate) fn prepare(
        &mut self,
        py: Python<'_>,
        project_root: &Path,
        files: &[PathBuf],
    ) -> PyResult<()> {
        match self {
            Self::Native(resolver) => resolver.prepare(project_root, files),
            Self::Custom(object) => custom_prepare(py, object, project_root, files),
            Self::Disabled => Ok(()),
        }
    }

    /// One answer per occurrence, in the same order.
    pub(crate) fn resolve_all(
        &self,
        py: Python<'_>,
        occurrences: &[(String, OccurrenceRef)],
    ) -> PyResult<Vec<Option<ResolvedRef>>> {
        match self {
            Self::Native(resolver) => Ok(occurrences
                .iter()
                .map(|(path, occurrence)| resolver.resolve(path, occurrence.line, occurrence.col))
                .collect()),
            Self::Custom(object) => custom_resolve_all(py, object, occurrences),
            Self::Disabled => Ok(vec![None; occurrences.len()]),
        }
    }

    pub(crate) fn status(&self, py: Python<'_>) -> PyResult<ResolverStatus> {
        match self {
            Self::Native(resolver) => Ok(resolver.status()),
            Self::Custom(object) => custom_status(py, object),
            Self::Disabled => Ok(ResolverStatus::Unavailable),
        }
    }
}

/// Calls a custom resolver's `prepare`, with `pathlib.Path` arguments.
///
/// Paths are `Path` rather than `str` because that is what a resolver written
/// against graphlens expects, and because a resolver almost always ends up
/// joining or comparing them.
pub(crate) fn custom_prepare(
    py: Python<'_>,
    resolver: &Py<PyAny>,
    project_root: &Path,
    files: &[PathBuf],
) -> PyResult<()> {
    let path_type = py.import("pathlib")?.getattr("Path")?;
    let root = path_type.call1((project_root.to_string_lossy().into_owned(),))?;
    let paths = PyList::empty(py);
    for file in files {
        paths.append(path_type.call1((file.to_string_lossy().into_owned(),))?)?;
    }
    resolver.bind(py).call_method1("prepare", (root, paths))?;
    Ok(())
}

/// Calls a custom resolver's `resolve_all` and coerces every answer.
///
/// A short return would silently shift every later answer onto the wrong
/// use-site, so the length is checked rather than zipped away.
pub(crate) fn custom_resolve_all(
    py: Python<'_>,
    resolver: &Py<PyAny>,
    occurrences: &[(String, OccurrenceRef)],
) -> PyResult<Vec<Option<ResolvedRef>>> {
    let path_type = py.import("pathlib")?.getattr("Path")?;
    let queries = PyList::empty(py);
    for (path, occurrence) in occurrences {
        let path = path_type.call1((path.as_str(),))?;
        queries.append((path, occurrence.line, occurrence.col))?;
    }
    let answers = resolver.bind(py).call_method1("resolve_all", (queries,))?;
    let mut refs = Vec::with_capacity(occurrences.len());
    for item in answers.try_iter()? {
        refs.push(coerce_ref(&item?)?);
    }
    if refs.len() != occurrences.len() {
        return Err(crate::error::AdapterError::new_err(format!(
            "resolver returned {} answers for {} queries; \
             resolve_all must answer every query, with None for the unknown ones",
            refs.len(),
            occurrences.len()
        )));
    }
    Ok(refs)
}

/// Reads a custom resolver's `status`, tolerating a plain string.
pub(crate) fn custom_status(py: Python<'_>, resolver: &Py<PyAny>) -> PyResult<ResolverStatus> {
    let value = resolver.bind(py).call_method0("status")?;
    Ok(ResolverStatus::coerce(&value, ResolverStatus::Unavailable))
}

/// Coerces an arbitrary resolver's answer into a native [`ResolvedRef`].
///
/// A `ResolvedRef` is taken as-is; anything else is read attribute by
/// attribute, so a `NamedTuple`, a dataclass or a mock all work. Missing
/// attributes become empty rather than an error: a resolver that knows a
/// definition's position but not its name is still useful, because the position
/// is what binds the edge.
pub(crate) fn coerce_ref(item: &Bound<'_, PyAny>) -> PyResult<Option<ResolvedRef>> {
    if item.is_none() {
        return Ok(None);
    }
    if let Ok(reference) = item.extract::<ResolvedRef>() {
        return Ok(Some(reference));
    }
    let attr_str = |name: &str| -> PyResult<String> {
        Ok(item
            .getattr(name)
            .ok()
            .filter(|v| !v.is_none())
            .map(|v| v.str())
            .transpose()?
            .map(|v| v.to_string())
            .unwrap_or_default())
    };
    let file_path = item
        .getattr("file_path")
        .ok()
        .filter(|v| !v.is_none())
        .map(|v| v.str())
        .transpose()?
        .map(|v| v.to_string());
    let attr_u32 = |name: &str| -> u32 {
        item.getattr(name)
            .ok()
            .and_then(|v| v.extract::<u32>().ok())
            .unwrap_or(0)
    };
    let origin = attr_str("origin")?;
    Ok(Some(ResolvedRef {
        full_name: attr_str("full_name")?,
        file_path,
        line: attr_u32("line"),
        col: attr_u32("col"),
        kind: attr_str("kind")?,
        origin: if origin.is_empty() { "unknown".to_string() } else { origin },
    }))
}
