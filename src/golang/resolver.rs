//! Резолвер символов Go поверх `go/packages` и `go/types`.
//!
//! В отличие от Python и TypeScript, стандартную библиотеку сюда не
//! вкомпилировать: у Go это исходники в GOROOT, и типизация без них
//! невозможна. Поэтому резолверу нужен установленный Go — но для
//! Go-проекта он и так есть, а `gopls` (который требует graphlens)
//! ставится отдельно.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_uint};
use std::path::{Path, PathBuf};
use std::process::Command;

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::python::ResolvedRef;
use crate::status::ResolverStatus;

unsafe extern "C" {
    fn callix_go_open(root: *const c_char) -> c_int;
    fn callix_go_close(handle: c_int);
    fn callix_ts_free(ptr: *mut c_char);
    fn callix_go_definition(
        handle: c_int,
        file: *const c_char,
        line: c_uint,
        col: c_uint,
    ) -> *mut c_char;
}

/// Сегмент кеша модулей: пути вида `.../pkg/mod/...`.
const MOD_CACHE: [&str; 2] = ["pkg", "mod"];

fn parse_answer(raw: &str) -> Option<(String, u32, u32)> {
    let mut parts = raw.split('\t');
    let path = parts.next()?;
    if path.is_empty() {
        return None;
    }
    Some((
        path.to_string(),
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

/// `go env GOROOT`; None, если Go не установлен.
fn detect_goroot() -> Option<PathBuf> {
    let output = Command::new("go").args(["env", "GOROOT"]).output().ok()?;
    let root = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

fn in_mod_cache(path: &Path) -> bool {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.windows(2).any(|w| w == MOD_CACHE)
}

#[gen_stub_pyclass]
// unsendable: сессия живёт в Go и привязана к своему хэндлу.
#[pyclass(module = "callix._core", unsendable)]
pub struct GoResolver {
    handle: c_int,
    root: Option<PathBuf>,
    goroot: Option<PathBuf>,
}

impl GoResolver {
    pub fn empty() -> Self {
        Self { handle: 0, root: None, goroot: detect_goroot() }
    }

    /// Происхождение цели по её пути.
    ///
    /// Сравнивается и исходный путь, и развёрнутый: симлинк может быть с
    /// любой стороны. graphlens разворачивает только путь цели, из-за чего
    /// на Debian-подобных установках (`/usr/lib/go-X/src` → `/usr/share/…`)
    /// вся стандартная библиотека попадает в `unknown`.
    fn classify(&self, path: &str) -> &'static str {
        let original = Path::new(path);
        let resolved = original
            .canonicalize()
            .unwrap_or_else(|_| original.to_path_buf());
        let matches = |base: &Path| {
            let base_resolved = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
            original.starts_with(base)
                || resolved.starts_with(base)
                || original.starts_with(&base_resolved)
                || resolved.starts_with(&base_resolved)
        };

        if in_mod_cache(&resolved) || in_mod_cache(original) {
            return "third_party";
        }
        if let Some(root) = &self.root
            && matches(root)
        {
            return "internal";
        }
        if let Some(goroot) = &self.goroot
            && matches(goroot)
        {
            return "stdlib";
        }
        "unknown"
    }

    fn resolve_one(&self, file: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        if self.handle <= 0 {
            return None;
        }
        let file = CString::new(file).ok()?;
        let answer = unsafe {
            let ptr = callix_go_definition(self.handle, file.as_ptr(), line, col);
            if ptr.is_null() {
                return None;
            }
            let answer = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            callix_ts_free(ptr);
            answer
        };

        let (path, line, col) = parse_answer(&answer)?;
        let origin = self.classify(&path);
        Some(ResolvedRef {
            full_name: String::new(),
            file_path: Some(path),
            line,
            col,
            kind: String::new(),
            origin: origin.to_string(),
        })
    }

    pub fn prepare_rust(&mut self, project_root: &Path) -> PyResult<()> {
        self.prepare(project_root.to_path_buf(), None)
    }

    pub fn resolve_rust(&self, path: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(path, line, col)
    }

    pub fn status_rust(&self) -> ResolverStatus {
        if self.handle > 0 {
            ResolverStatus::Ok
        } else {
            ResolverStatus::Unavailable
        }
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl GoResolver {
    #[new]
    fn new() -> Self {
        Self::empty()
    }

    /// Загружает и типизирует пакеты проекта.
    ///
    /// Список файлов не нужен: `go/packages` сам разбирает `go.mod` и
    /// подтягивает зависимости.
    #[pyo3(signature = (project_root, files = None))]
    fn prepare(&mut self, project_root: PathBuf, files: Option<Vec<PathBuf>>) -> PyResult<()> {
        let _ = files;
        let root = project_root.canonicalize().unwrap_or(project_root);
        let c_root = CString::new(root.to_string_lossy().as_ref())?;

        if self.handle > 0 {
            unsafe { callix_go_close(self.handle) };
        }
        self.handle = unsafe { callix_go_open(c_root.as_ptr()) };
        self.root = Some(root);
        Ok(())
    }

    fn resolve_all(&self, queries: Vec<(PathBuf, u32, u32)>) -> Vec<Option<ResolvedRef>> {
        queries
            .into_iter()
            .map(|(path, line, col)| self.resolve_one(&path.to_string_lossy(), line, col))
            .collect()
    }

    fn definition_at(&self, file: PathBuf, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(&file.to_string_lossy(), line, col)
    }

    fn status(&self) -> ResolverStatus {
        self.status_rust()
    }

    fn __repr__(&self) -> String {
        format!(
            "GoResolver(root={:?}, goroot={:?}, ready={})",
            self.root.as_ref().map(|r| r.to_string_lossy()),
            self.goroot.as_ref().map(|r| r.to_string_lossy()),
            self.handle > 0
        )
    }
}

impl Drop for GoResolver {
    fn drop(&mut self) {
        if self.handle > 0 {
            unsafe { callix_go_close(self.handle) };
        }
    }
}
