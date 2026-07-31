//! The Rust symbol resolver, built on the batch `rust-analyzer scip` index.
//!
//! An interactive LSP server keeps the whole workspace's analysis state
//! resident and balloons to tens of gigabytes on large projects. A SCIP index
//! is written once, read statically, and answers queries from in-memory
//! tables — which is why it is used here instead of the LSP.
//!
//! As with Go, the language cannot be compiled in wholesale: `rust-analyzer`
//! (and Cargo) must be installed — but a Rust project has them anyway.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::python::ResolvedRef;
use crate::status::ResolverStatus;

use crate::scip::{ScipAnswer, ScipIndex};

/// The ceiling on one `rust-analyzer scip` run. Large workspaces finish well
/// under it; the cap only guards against a hang.
const SCIP_TIMEOUT: Duration = Duration::from_secs(1800);

/// Crates of the Rust standard distribution as they appear in a SCIP symbol.
/// A cargo symbol reads `<scheme> cargo <package> <version> <descriptors>`.
const STD_PACKAGES: [&str; 5] = ["std", "core", "alloc", "proc_macro", "test"];

/// A cargo symbol needs at least `<scheme> <manager> <package> <version>`.
const SCIP_SYMBOL_MIN_PARTS: usize = 4;

/// An external symbol's origin is read straight from its scheme.
///
/// That is more robust than guessing from the definition's file path, and it
/// works even when the crate's sources were never opened.
fn symbol_origin(symbol: &str) -> &'static str {
    let parts: Vec<&str> = symbol.splitn(5, ' ').collect();
    if parts.len() < SCIP_SYMBOL_MIN_PARTS || parts[1] != "cargo" {
        return "unknown";
    }
    if STD_PACKAGES.contains(&parts[2]) {
        "stdlib"
    } else {
        "third_party"
    }
}

/// The first executable with this name on PATH.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// The concrete rust-analyzer path rustup resolves from `cwd`.
fn rustup_which(rustup: &Path, cwd: &Path) -> Option<PathBuf> {
    let output = Command::new(rustup)
        .args(["which", "rust-analyzer"])
        .current_dir(cwd)
        // A pin from the environment would override the project's
        // rust-toolchain.toml.
        .env_remove("RUSTUP_TOOLCHAIN")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    path.is_file().then_some(path)
}

/// Which rust-analyzer binary to spawn for this project.
///
/// `rust-analyzer` on PATH is usually not a binary but a rustup proxy that
/// honours the project's `rust-toolchain.toml`. Two cases follow:
///
/// 1. The pinned toolchain has the component — that is the one we want: a
///    build matching the project's toolchain analyses it correctly. So we ask
///    `rustup which` from the project root first.
/// 2. The pinned toolchain lacks the component (ruff, for instance, pins 1.96
///    without rust-analyzer) — the proxy would exit with `Unknown binary` and
///    resolution would silently yield nothing. Then we fall back to the
///    default toolchain, asking from a neutral directory.
fn resolve_ra_binary(project_root: &Path) -> PathBuf {
    let fallback = || which("rust-analyzer").unwrap_or_else(|| PathBuf::from("rust-analyzer"));
    match which("rustup") {
        Some(rustup) => rustup_which(&rustup, project_root)
            .or_else(|| rustup_which(&rustup, &std::env::temp_dir()))
            .unwrap_or_else(fallback),
        None => fallback(),
    }
}

/// Waits for the process, killing it on timeout.
///
/// Returns the exit code (None when the process had to be killed).
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct RustResolver {
    root: Option<PathBuf>,
    status: ResolverStatus,
    index: ScipIndex,
}

impl Default for RustResolver {
    fn default() -> Self {
        Self { root: None, status: ResolverStatus::Unavailable, index: ScipIndex::new() }
    }
}

impl RustResolver {
    pub fn empty() -> Self {
        Self::default()
    }

    fn reset(&mut self) {
        self.index = ScipIndex::new();
    }

    /// Runs `rust-analyzer scip`; returns `(index bytes, exit code)`.
    fn run_scip(&self, project_root: &Path) -> (Option<Vec<u8>>, Option<i32>) {
        let binary = resolve_ra_binary(project_root);
        // temp_dir honours TMPDIR: a large workspace's index weighs hundreds
        // of megabytes, and on a small /tmp it is worth moving elsewhere.
        let output_path = std::env::temp_dir().join(format!("callix-{}.scip", std::process::id()));

        let spawned = Command::new(&binary)
            .arg("scip")
            .arg(project_root)
            .arg("--output")
            .arg(&output_path)
            .current_dir(project_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        let Ok(mut child) = spawned else {
            return (None, None);
        };
        let code = wait_with_timeout(&mut child, SCIP_TIMEOUT);

        let data = fs::read(&output_path)
            .ok()
            .filter(|bytes| !bytes.is_empty());
        let _ = fs::remove_file(&output_path);
        (data, code)
    }

    fn definition_ref(&self, doc: u32, line: u32, col: u32) -> ResolvedRef {
        let root = self.root.as_ref().expect("root is set during prepare");
        ResolvedRef {
            full_name: String::new(),
            file_path: Some(root.join(self.index.doc_path(doc)).to_string_lossy().into_owned()),
            line: line + 1,
            col: col + 1,
            kind: String::new(),
            origin: "internal".to_string(),
        }
    }

    /// An absolute path → a path relative to the index root.
    ///
    /// SCIP's `relative_path` always uses forward slashes, so this does too.
    fn relative(&self, file: &Path) -> String {
        let Some(root) = &self.root else {
            return file.to_string_lossy().into_owned();
        };
        let resolved = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
        resolved
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| file.to_string_lossy().into_owned())
    }

    fn resolve_one(&self, file: &Path, line: u32, col: u32) -> Option<ResolvedRef> {
        self.root.as_ref()?;
        let relative = self.relative(file);
        match self.index.lookup(&relative, line.checked_sub(1)?, col.checked_sub(1)?)? {
            ScipAnswer::Definition { doc, line, col } => Some(self.definition_ref(doc, line, col)),
            ScipAnswer::External { symbol } => Some(ResolvedRef {
                full_name: symbol.clone(),
                file_path: None,
                line: 0,
                col: 0,
                kind: String::new(),
                origin: symbol_origin(&symbol).to_string(),
            }),
        }
    }

    pub fn prepare_rust(&mut self, project_root: &Path) {
        self.prepare(project_root.to_path_buf(), None);
    }

    pub fn resolve_rust(&self, path: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(Path::new(path), line, col)
    }

    pub fn status_rust(&self) -> ResolverStatus {
        self.status
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl RustResolver {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    /// Builds the SCIP index for the workspace.
    ///
    /// No file list is needed: `rust-analyzer` parses `Cargo.toml` and indexes
    /// the whole workspace itself.
    #[pyo3(signature = (project_root, files = None))]
    fn prepare(&mut self, project_root: PathBuf, files: Option<Vec<PathBuf>>) {
        let _ = files;
        let root = project_root.canonicalize().unwrap_or(project_root);
        self.root = Some(root.clone());
        self.status = ResolverStatus::Unavailable;
        self.reset();

        let (data, code) = self.run_scip(&root);
        let Some(data) = data else {
            return;
        };
        self.index.ingest(&data);

        self.status = if self.index.is_empty() {
            ResolverStatus::Degraded
        } else if code != Some(0) {
            // rust-analyzer errored mid-run (a crate failed to load, say) but
            // left a partial index: report degraded so strict mode does not
            // mistake an incomplete graph for a complete one.
            ResolverStatus::Degraded
        } else {
            ResolverStatus::Ok
        };
    }

    fn resolve_all(&self, queries: Vec<(PathBuf, u32, u32)>) -> Vec<Option<ResolvedRef>> {
        queries
            .into_iter()
            .map(|(path, line, col)| self.resolve_one(&path, line, col))
            .collect()
    }

    fn definition_at(&self, file: PathBuf, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(&file, line, col)
    }

    fn status(&self) -> ResolverStatus {
        self.status
    }

    fn __repr__(&self) -> String {
        format!(
            "RustResolver(root={:?}, documents={}, symbols={}, status={})",
            self.root.as_ref().map(|r| r.to_string_lossy()),
            self.index.doc_count(),
            self.index.symbol_count(),
            self.status.as_str()
        )
    }
}

/// `rust-analyzer scip` indexes the whole workspace in one subprocess run, so
/// the file list is not passed on. `prepare` cannot fail: a missing
/// rust-analyzer, a workspace that will not load and an unreadable index all
/// leave the resolver in a status the adapter reports rather than raising, so
/// that a project with one broken crate still yields its structural graph.
impl crate::resolver_slot::NativeResolver for RustResolver {
    fn prepare(&mut self, project_root: &Path, _files: &[PathBuf]) -> PyResult<()> {
        self.prepare_rust(project_root);
        Ok(())
    }

    fn resolve(&self, path: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_rust(path, line, col)
    }

    fn status(&self) -> ResolverStatus {
        self.status_rust()
    }
}
