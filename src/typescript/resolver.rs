//! Резолвер символов TypeScript поверх typescript-go.
//!
//! Компилятор слинкован в модуль статически: `tsc`, `tsgo` и Node в
//! системе не нужны, `lib.d.ts` тоже внутри. Мост на стороне Go живёт в
//! `go/bridge.go`, здесь — только вызовы через FFI и разбор ответа.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_uint};
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::python::ResolvedRef;
use crate::status::ResolverStatus;

unsafe extern "C" {
    fn callix_ts_open(root: *const c_char) -> c_int;
    fn callix_ts_close(handle: c_int);
    fn callix_ts_free(ptr: *mut c_char);
    fn callix_ts_definition(
        handle: c_int,
        file: *const c_char,
        line: c_uint,
        col: c_uint,
    ) -> *mut c_char;
}

/// Ответ моста: `путь\tстрока\tколонка` либо пустая строка.
fn parse_answer(raw: &str) -> Option<(String, u32, u32)> {
    let mut parts = raw.split('\t');
    let path = parts.next()?;
    if path.is_empty() {
        return None;
    }
    let line = parts.next()?.parse().ok()?;
    let col = parts.next()?.parse().ok()?;
    Some((path.to_string(), line, col))
}

#[gen_stub_pyclass]
// unsendable: сессия живёт в Go и привязана к своему хэндлу.
#[pyclass(module = "callix._core", unsendable)]
pub struct TsResolver {
    handle: c_int,
    root: Option<PathBuf>,
}

impl TsResolver {
    /// Резолвер без поднятой сессии — для вызова из Rust.
    pub fn empty() -> Self {
        Self { handle: 0, root: None }
    }

    pub fn prepare_rust(&mut self, project_root: &Path) -> PyResult<()> {
        self.prepare(project_root.to_path_buf(), None)
    }

    pub fn resolve_rust(&self, path: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(path, line, col)
    }

    pub fn status_rust(&self) -> ResolverStatus {
        if self.handle > 0 { ResolverStatus::Ok } else { ResolverStatus::Unavailable }
    }

    /// Происхождение цели по её пути.
    fn classify(&self, path: &str) -> &'static str {
        // Встроенные библиотеки ts отдаются под своей схемой, файла на
        // диске у них нет.
        if path.starts_with("bundled:") {
            return "stdlib";
        }
        let path = Path::new(path);
        if path.components().any(|c| c.as_os_str() == "node_modules") {
            return "third_party";
        }
        if let Some(root) = &self.root
            && path.starts_with(root)
        {
            return "internal";
        }
        "unknown"
    }

    fn resolve_one(&self, file: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        if self.handle <= 0 {
            return None;
        }
        let file = CString::new(file).ok()?;
        let answer = unsafe {
            let ptr = callix_ts_definition(self.handle, file.as_ptr(), line, col);
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
            file_path: if origin == "stdlib" && path.starts_with("bundled:") {
                None
            } else {
                Some(path)
            },
            line,
            col,
            kind: String::new(),
            origin: origin.to_string(),
        })
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl TsResolver {
    #[new]
    fn new() -> Self {
        Self { handle: 0, root: None }
    }

    /// Поднимает проект: typescript-go сам находит tsconfig.json и строит
    /// программу, поэтому список файлов ему не передаётся.
    #[pyo3(signature = (project_root, files = None))]
    fn prepare(&mut self, project_root: PathBuf, files: Option<Vec<PathBuf>>) -> PyResult<()> {
        let _ = files;
        let root = project_root
            .canonicalize()
            .unwrap_or(project_root);
        let c_root = CString::new(root.to_string_lossy().as_ref())?;

        if self.handle > 0 {
            unsafe { callix_ts_close(self.handle) };
        }
        self.handle = unsafe { callix_ts_open(c_root.as_ptr()) };
        self.root = Some(root);
        Ok(())
    }

    /// Батч позиций → определения, порядок сохраняется.
    fn resolve_all(&self, queries: Vec<(PathBuf, u32, u32)>) -> Vec<Option<ResolvedRef>> {
        queries
            .into_iter()
            .map(|(path, line, col)| self.resolve_one(&path.to_string_lossy(), line, col))
            .collect()
    }

    /// Одна позиция → определение.
    fn definition_at(&self, file: PathBuf, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(&file.to_string_lossy(), line, col)
    }

    fn status(&self) -> ResolverStatus {
        if self.handle > 0 {
            ResolverStatus::Ok
        } else {
            ResolverStatus::Unavailable
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "TsResolver(root={:?}, ready={})",
            self.root.as_ref().map(|r| r.to_string_lossy()),
            self.handle > 0
        )
    }
}

impl Drop for TsResolver {
    fn drop(&mut self) {
        if self.handle > 0 {
            unsafe { callix_ts_close(self.handle) };
        }
    }
}
