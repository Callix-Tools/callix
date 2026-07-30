//! The Rust symbol resolver, built on the batch `rust-analyzer scip` index.
//!
//! An interactive LSP server keeps the whole workspace's analysis state
//! resident and balloons to tens of gigabytes on large projects. A SCIP index
//! is written once, read statically, and answers queries from in-memory
//! tables — which is why it is used here instead of the LSP.
//!
//! As with Go, the language cannot be compiled in wholesale: `rust-analyzer`
//! (and Cargo) must be installed — but a Rust project has them anyway.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::python::ResolvedRef;
use crate::status::ResolverStatus;

use super::scip::{ROLE_DEFINITION, for_each_document};

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

/// A definition's location: the document index and 0-based coordinates.
type Location = (u32, u32, u32);

#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct RustResolver {
    root: Option<PathBuf>,
    status: ResolverStatus,
    /// The index's document paths and the reverse lookup over them.
    docs: Vec<String>,
    doc_index: HashMap<String, u32>,
    /// The symbol pool: one string for many occurrences.
    symbols: Vec<String>,
    symbol_index: HashMap<String, u32>,
    /// Document → `(line, column)` → symbol. Coordinates are 0-based.
    by_doc: Vec<HashMap<(u32, u32), u32>>,
    /// Global symbol → the location of its definition.
    defs: HashMap<u32, Location>,
    /// Document → a `local …` symbol → its definition site within the
    /// document.
    local_defs: Vec<HashMap<u32, (u32, u32)>>,
}

impl Default for RustResolver {
    fn default() -> Self {
        Self {
            root: None,
            status: ResolverStatus::Unavailable,
            docs: Vec::new(),
            doc_index: HashMap::new(),
            symbols: Vec::new(),
            symbol_index: HashMap::new(),
            by_doc: Vec::new(),
            defs: HashMap::new(),
            local_defs: Vec::new(),
        }
    }
}

impl RustResolver {
    pub fn empty() -> Self {
        Self::default()
    }

    fn intern_symbol(&mut self, symbol: &str) -> u32 {
        if let Some(index) = self.symbol_index.get(symbol) {
            return *index;
        }
        let index = self.symbols.len() as u32;
        self.symbols.push(symbol.to_string());
        self.symbol_index.insert(symbol.to_string(), index);
        index
    }

    fn intern_doc(&mut self, relative_path: &str) -> u32 {
        if let Some(index) = self.doc_index.get(relative_path) {
            return *index;
        }
        let index = self.docs.len() as u32;
        self.docs.push(relative_path.to_string());
        self.doc_index.insert(relative_path.to_string(), index);
        self.by_doc.push(HashMap::new());
        self.local_defs.push(HashMap::new());
        index
    }

    fn reset(&mut self) {
        self.docs.clear();
        self.doc_index.clear();
        self.symbols.clear();
        self.symbol_index.clear();
        self.by_doc.clear();
        self.defs.clear();
        self.local_defs.clear();
    }

    /// Folds a SCIP index into the lookup tables.
    fn ingest(&mut self, data: &[u8]) {
        for_each_document(data, |relative_path, occurrences| {
            let mut doc: Option<u32> = None;
            for occurrence in occurrences {
                if occurrence.symbol.is_empty() {
                    continue;
                }
                // A document is only created once it has something to find.
                let doc = *doc.get_or_insert_with(|| self.intern_doc(&relative_path));
                let is_local = occurrence.symbol.starts_with("local ");
                let index = self.intern_symbol(&occurrence.symbol);
                let position = (occurrence.start_line, occurrence.start_col);
                // Last one wins here while `defs` below keeps the first, and
                // the asymmetry is deliberate rather than an oversight. Two
                // occurrences can share a start position — a derive or a macro
                // expansion maps several symbols onto one source range — and
                // there is no principled way to prefer one, so the tie-break is
                // arbitrary either way. `defs` keeps the first because a
                // symbol's definition site should not depend on how many
                // references follow it.
                self.by_doc[doc as usize].insert(position, index);
                if occurrence.roles & ROLE_DEFINITION == 0 {
                    continue;
                }
                if is_local {
                    self.local_defs[doc as usize].entry(index).or_insert(position);
                } else {
                    self.defs.entry(index).or_insert((doc, position.0, position.1));
                }
            }
        });
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

    fn definition_ref(&self, doc: u32, line: u32, col: u32, origin: &str) -> ResolvedRef {
        let root = self.root.as_ref().expect("root is set during prepare");
        ResolvedRef {
            full_name: String::new(),
            file_path: Some(root.join(&self.docs[doc as usize]).to_string_lossy().into_owned()),
            line: line + 1,
            col: col + 1,
            kind: String::new(),
            origin: origin.to_string(),
        }
    }

    /// A symbol → its definition, or an external reference.
    fn symbol_to_ref(&self, symbol: u32, doc: u32) -> Option<ResolvedRef> {
        let name = &self.symbols[symbol as usize];
        if name.starts_with("local ") {
            let (line, col) = *self.local_defs[doc as usize].get(&symbol)?;
            return Some(self.definition_ref(doc, line, col, "internal"));
        }
        if let Some((target_doc, line, col)) = self.defs.get(&symbol) {
            return Some(self.definition_ref(*target_doc, *line, *col, "internal"));
        }
        Some(ResolvedRef {
            full_name: name.clone(),
            file_path: None,
            line: 0,
            col: 0,
            kind: String::new(),
            origin: symbol_origin(name).to_string(),
        })
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
        let doc = *self.doc_index.get(&self.relative(file))?;
        let symbol = *self.by_doc[doc as usize].get(&(line.checked_sub(1)?, col.checked_sub(1)?))?;
        self.symbol_to_ref(symbol, doc)
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
        self.ingest(&data);

        self.status = if self.docs.is_empty() {
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
            self.docs.len(),
            self.symbols.len(),
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
