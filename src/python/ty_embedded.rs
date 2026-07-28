//! The embedded ty resolver: the same type checker, but linked into the
//! module instead of spawned as a `ty server` subprocess.
//!
//! Correspondence with `ty server`: `goto_definition` is the same thing as
//! `textDocument/definition`, positions are counted in UTF-16 (the LSP
//! default encoding, which is what the Python client negotiated), and origin
//! classification mirrors `TyResolver._classify`.

use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};
use ruff_db::files::{FilePath, system_path_to_file};
use ruff_db::source::{line_index, source_text};
use ruff_db::system::{OsSystem, SystemPath, SystemPathBuf};
use ruff_source_file::{OneIndexed, PositionEncoding, SourceLocation};
use ty_ide::goto_definition;
use ty_project::{ProjectDatabase, ProjectMetadata};

use super::adapter::ResolvedRef;
use crate::status::ResolverStatus;

/// LSP positions default to UTF-16; the Python client never advertised
/// anything else, so we keep the same encoding.
const ENCODING: PositionEncoding = PositionEncoding::Utf16;

#[gen_stub_pyclass]
// unsendable: the salsa database is not Sync, so the resolver stays on one
// thread
#[pyclass(module = "callix._core", unsendable)]
pub struct EmbeddedTyResolver {
    db: Option<ProjectDatabase>,
    root: Option<PathBuf>,
    /// `sys.base_prefix` — used to recognize stdlib outside typeshed.
    base_prefix: PathBuf,
}

impl EmbeddedTyResolver {
    /// A resolver for the running interpreter: stdlib is recognized by its
    /// `sys.base_prefix`.
    pub fn for_running_python(py: Python<'_>) -> PyResult<Self> {
        let base_prefix: String = py.import("sys")?.getattr("base_prefix")?.extract()?;
        Ok(Self { db: None, root: None, base_prefix: PathBuf::from(base_prefix) })
    }

    pub fn prepare_rust(&mut self, project_root: &str) -> PyResult<()> {
        self.prepare(project_root, None)
    }

    pub fn resolve_rust(&self, path: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(path, line, col)
    }

    pub fn status_rust(&self) -> ResolverStatus {
        if self.db.is_some() { ResolverStatus::Ok } else { ResolverStatus::Unavailable }
    }

    /// Mirrors graphlens's `TyResolver._classify`.
    fn classify(&self, file_path: Option<&Path>) -> &'static str {
        let Some(path) = file_path else {
            return "stdlib";
        };
        let parts: Vec<&str> = path
            .components()
            .map(|c| c.as_os_str().to_str().unwrap_or(""))
            .collect();
        if parts.contains(&"typeshed") {
            return "stdlib";
        }
        if parts.contains(&"site-packages") || parts.contains(&"dist-packages") {
            return "third_party";
        }
        if let Some(root) = &self.root
            && path.starts_with(root)
        {
            return "internal";
        }
        // Symlinks are resolved: uv keeps its Pythons behind them, while ty
        // hands back the real paths.
        let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let base = self
            .base_prefix
            .canonicalize()
            .unwrap_or_else(|_| self.base_prefix.clone());
        if real.starts_with(&base) {
            return "stdlib";
        }
        "unknown"
    }

    /// One position → a definition. Any miss yields None, as in the LSP
    /// version.
    fn resolve_one(&self, path: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        let db = self.db.as_ref()?;
        let system_path = SystemPath::new(path);
        let file = system_path_to_file(db, system_path).ok()?;

        let text = source_text(db, file);
        let index = line_index(db, file);
        let offset = index.offset(
            SourceLocation {
                line: OneIndexed::new(line as usize)?,
                character_offset: OneIndexed::new(col as usize)?,
            },
            &text,
            ENCODING,
        );

        let targets = goto_definition(db, file, offset)?;
        let target = targets.value.into_iter().next()?;

        let target_file = target.file();
        let target_path = match target_file.path(db) {
            FilePath::System(p) => Some(PathBuf::from(p.as_str())),
            // Files from the bundled typeshed have no path on disk.
            FilePath::Vendored(_) | FilePath::SystemVirtual(_) => None,
        };

        let target_text = source_text(db, target_file);
        let target_index = line_index(db, target_file);
        let location =
            target_index.source_location(target.focus_range().start(), &target_text, ENCODING);

        Some(ResolvedRef {
            full_name: String::new(),
            file_path: target_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            line: location.line.get() as u32,
            col: location.character_offset.get() as u32,
            kind: String::new(),
            origin: self.classify(target_path.as_deref()).to_string(),
        })
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl EmbeddedTyResolver {
    #[new]
    #[pyo3(signature = (base_prefix))]
    fn new(base_prefix: String) -> Self {
        Self { db: None, root: None, base_prefix: PathBuf::from(base_prefix) }
    }

    /// Brings up ty's database for the project. `files` is unused: ty walks
    /// the project itself according to its own configuration — unlike the LSP
    /// version, which had to open files one at a time.
    #[pyo3(signature = (project_root, files = None))]
    fn prepare(&mut self, project_root: &str, files: Option<Vec<String>>) -> PyResult<()> {
        let _ = files;
        let root = SystemPathBuf::from(project_root);
        if !root.as_std_path().is_absolute() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "project_root must be an absolute path",
            ));
        }
        let system = OsSystem::new(&root);
        let metadata = ProjectMetadata::discover(&root, &system).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("ty could not read the project: {e}"))
        })?;
        self.db = Some(ProjectDatabase::use_defaults(metadata, system));
        self.root = Some(PathBuf::from(project_root));
        Ok(())
    }

    /// A batch of positions → definitions, order preserved.
    fn resolve_all(&self, queries: Vec<(String, u32, u32)>) -> Vec<Option<ResolvedRef>> {
        queries
            .into_iter()
            .map(|(path, line, col)| self.resolve_one(&path, line, col))
            .collect()
    }

    /// One position → a definition.
    fn definition_at(&self, file: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(file, line, col)
    }

    fn status(&self) -> ResolverStatus {
        if self.db.is_some() {
            ResolverStatus::Ok
        } else {
            ResolverStatus::Unavailable
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "EmbeddedTyResolver(root={:?}, ready={})",
            self.root.as_ref().map(|r| r.to_string_lossy()),
            self.db.is_some()
        )
    }
}
