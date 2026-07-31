//! An opt-in C/C++ resolver backed by `scip-clang`, a real semantic index.
//!
//! [`CFamilyResolver`](super::resolver::CFamilyResolver) — the default — is a
//! symbol table: it does not evaluate the preprocessor, does not expand
//! macros, and cannot pick an overload, because none of those are answerable
//! without a real compiler frontend. `scip-clang` **is** one: it drives Clang
//! itself over a project's actual compile commands, so a call to an
//! overloaded function binds to the specific overload Clang picked, not to
//! "the first declaration sharing this bare name."
//!
//! The cost is exactly what the symbol table exists to work around: a
//! `compile_commands.json`. It is a build artifact, not something derivable
//! from sources, and empirically it is checked into almost no real
//! repository. That used to be the whole reason this backend was opt-in only;
//! it no longer is. `CAdapter`/`CppAdapter` now try this automatically
//! whenever `resolve=True` and no `resolver=` was given — see
//! `analyze_family` in `adapter.rs` — and fall back to the symbol table only
//! when this reports `Unavailable` (no `scip-clang` on `PATH`, or no compdb at
//! the project root). A caller who wants it explicitly, or at a non-default
//! compdb path, still can:
//!
//! ```python
//! CppAdapter(resolver=ClangScipResolver(compdb_path="build/compile_commands.json")).analyze(root)
//! ```
//!
//! `prepare`/`resolve_all`/`status` is the same protocol a custom Python
//! resolver would implement, so passing an instance explicitly still goes
//! through the ordinary `resolver=` path — the auto-detection is the only
//! place this type is treated specially, and it is driven natively (through
//! [`crate::resolver_slot::NativeResolver`], see the impl at the bottom of
//! this file) rather than through Python, for the same reason `RustResolver`
//! and the other three position-keyed backends are.
//!
//! Architecturally this is the C-family counterpart to
//! [`RustResolver`](crate::rustlang::RustResolver): both drive an external
//! indexer once per `prepare` and decode the SCIP index it writes through
//! [`crate::scip::ScipIndex`], which turned out to need no changes at all —
//! `scip-clang`'s `local N` scope-local symbols and its position-keyed names
//! for macros and other things with no stable identifier are exactly the
//! shapes `rust-analyzer scip` already produces. The one thing that could not
//! be shared is the symbol *scheme*: a `rust-analyzer` symbol reads
//! `rust-analyzer cargo callix 0.1.0 ids/node_id().`, a `scip-clang` one reads
//! `cxx . . $ leveldb/Arena#Allocate(4b58d6ff543a4736).` — different enough
//! that classifying an unresolved symbol's origin has to live here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::python::ResolvedRef;
use crate::scip::{ScipAnswer, ScipIndex};
use crate::status::ResolverStatus;

/// The ceiling on one `scip-clang` run. Large codebases (postgres, an
/// interpreter) still finish in under a minute; the cap only guards a hang.
const SCIP_CLANG_TIMEOUT: Duration = Duration::from_secs(1800);

/// Top-level symbol namespaces that belong to a standard library rather than
/// to project code or a third-party dependency — a coarse, best-effort
/// classification, the same spirit as `RustResolver`'s package-name check.
const STDLIB_PREFIXES: [&str; 4] = ["std/", "__gnu_cxx/", "__cxxabiv1/", "__cxx11/"];

/// An external symbol's origin, read from its leading namespace segment.
fn symbol_origin(symbol: &str) -> &'static str {
    // The descriptor is everything after the `cxx . . $ ` scheme prefix; a
    // malformed or unexpectedly-shaped symbol just falls through to unknown.
    let Some(descriptor) = symbol.splitn(5, ' ').nth(4) else {
        return "unknown";
    };
    if STDLIB_PREFIXES.iter().any(|prefix| descriptor.starts_with(prefix)) {
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

/// Waits for the process, killing it on timeout. Mirrors the Rust resolver's
/// helper of the same shape — small enough that sharing it would cost more
/// than it saves.
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
pub struct ClangScipResolver {
    /// Where the compilation database is, relative to `project_root` unless
    /// absolute. Defaults to the project root's own `compile_commands.json` —
    /// most repositories that have one keep it there or symlink it there,
    /// `cmake --build` included.
    compdb_path: PathBuf,
    root: Option<PathBuf>,
    status: ResolverStatus,
    index: ScipIndex,
}

impl ClangScipResolver {
    fn reset(&mut self) {
        self.index = ScipIndex::new();
    }

    /// Runs `scip-clang`; returns `(index bytes, exit code)`. `None, None`
    /// covers both "the binary is not installed" and "it could not be
    /// spawned" — indistinguishable to the caller and both mean the same
    /// thing: `unavailable`.
    fn run_scip_clang(&self, project_root: &Path, compdb: &Path) -> (Option<Vec<u8>>, Option<i32>) {
        let Some(binary) = which("scip-clang") else {
            return (None, None);
        };
        // temp_dir honours TMPDIR: a large codebase's index can weigh
        // hundreds of megabytes, and on a small /tmp it is worth moving
        // elsewhere.
        let output_path =
            std::env::temp_dir().join(format!("callix-clang-{}.scip", std::process::id()));

        let spawned = Command::new(&binary)
            .arg("--compdb-path")
            .arg(compdb)
            .arg("--index-output-path")
            .arg(&output_path)
            .arg("--no-progress-report")
            // Relative document paths in the index are relative to this
            // directory: `apply_resolutions_inner` strips `project_root` off
            // an absolute `file_path` to match a node, so the two have to
            // agree, and `project_root` is the only honest choice — it is
            // what the rest of the graph's paths are already relative to.
            .current_dir(project_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        let Ok(mut child) = spawned else {
            return (None, None);
        };
        let code = wait_with_timeout(&mut child, SCIP_CLANG_TIMEOUT);

        let data = fs::read(&output_path).ok().filter(|bytes| !bytes.is_empty());
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

    pub(crate) fn prepare_rust(&mut self, project_root: &Path) {
        self.prepare(project_root.to_path_buf(), None);
    }

    pub(crate) fn resolve_rust(&self, path: &str, line: u32, col: u32) -> Option<ResolvedRef> {
        self.resolve_one(Path::new(path), line, col)
    }

    pub(crate) fn status_rust(&self) -> ResolverStatus {
        self.status
    }
}

#[gen_stub_pymethods]
#[pymethods]
impl ClangScipResolver {
    /// `compdb_path` is relative to the project root passed to `prepare`
    /// unless given as an absolute path — pass it explicitly whenever the
    /// build directory is not the project root itself, which for an
    /// out-of-tree CMake build (`cmake -B build`) it never is.
    #[new]
    #[pyo3(signature = (compdb_path = None))]
    pub(crate) fn new(compdb_path: Option<PathBuf>) -> Self {
        Self {
            compdb_path: compdb_path.unwrap_or_else(|| PathBuf::from("compile_commands.json")),
            root: None,
            status: ResolverStatus::Unavailable,
            index: ScipIndex::new(),
        }
    }

    /// Runs `scip-clang` over the project's compilation database.
    ///
    /// `files` is accepted and ignored: `scip-clang` reads the compilation
    /// database itself and indexes exactly the translation units it lists,
    /// the same reason `rust-analyzer scip` ignores the file list callix
    /// already collected.
    #[pyo3(signature = (project_root, files = None))]
    fn prepare(&mut self, project_root: PathBuf, files: Option<Vec<PathBuf>>) {
        let _ = files;
        let root = project_root.canonicalize().unwrap_or(project_root);
        self.root = Some(root.clone());
        self.status = ResolverStatus::Unavailable;
        self.reset();

        let compdb = if self.compdb_path.is_absolute() {
            self.compdb_path.clone()
        } else {
            root.join(&self.compdb_path)
        };
        if !compdb.is_file() {
            // No compilation database, no index — this is the ordinary case
            // for a repository that never generated one, not an error.
            return;
        }

        let (data, code) = self.run_scip_clang(&root, &compdb);
        let Some(data) = data else {
            return;
        };
        self.index.ingest(&data);

        self.status = if self.index.is_empty() {
            ResolverStatus::Degraded
        } else if code != Some(0) {
            // scip-clang errored on some translation units but still wrote a
            // partial index: report degraded so strict mode does not mistake
            // an incomplete graph for a complete one.
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
            "ClangScipResolver(root={:?}, documents={}, symbols={}, status={})",
            self.root.as_ref().map(|r| r.to_string_lossy()),
            self.index.doc_count(),
            self.index.symbol_count(),
            self.status.as_str()
        )
    }
}

/// Lets `CAdapter`/`CppAdapter` drive this resolver directly, the way
/// `RustResolver` and the other three position-keyed backends are driven —
/// used for the auto-detection in `adapter.rs`: try this first, and only fall
/// back to the symbol table if it reports `Unavailable`.
impl crate::resolver_slot::NativeResolver for ClangScipResolver {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A real symbol, as `scip-clang` 0.4.0 actually emits it — verified
    /// against a live run over google/leveldb, not guessed from the SCIP spec.
    const REAL_SYMBOL: &str = "cxx . . $ leveldb/Arena#AllocateNewBlock(4b58d6ff543a4736).";

    #[test]
    fn classifies_libstdcxx_internals_as_stdlib() {
        for symbol in [
            "cxx . . $ std/vector#`operator[]`(428bb168810b9e52).",
            "cxx . . $ __gnu_cxx/__normal_iterator#operator++(93f2e9c3bc4efc49).",
            "cxx . . $ __cxxabiv1/__class_type_info#foo().",
        ] {
            assert_eq!(symbol_origin(symbol), "stdlib", "{symbol}");
        }
    }

    #[test]
    fn classifies_project_and_third_party_namespaces_as_third_party() {
        // A symbol scip-clang builds from the project's own sources is
        // indistinguishable, by namespace alone, from one in a vendored
        // dependency — both fall through to the same bucket, the same way
        // RustResolver's package-name check cannot tell a workspace crate from
        // a path dependency.
        assert_eq!(symbol_origin(REAL_SYMBOL), "third_party");
        assert_eq!(symbol_origin("cxx . . $ boost/system#error()."), "third_party");
    }

    #[test]
    fn a_malformed_symbol_is_unknown_rather_than_panicking() {
        // Fewer than five space-separated fields means there is no
        // descriptor to classify at all — not even enough of the scheme to
        // guess wrong. `nth(4)` on any *five or more* tokens is unlikely
        // ever to be attempted with such a short, clearly-not-SCIP string, so
        // this is the honest floor, not a full validation of the scheme.
        for symbol in ["", "cxx", "cxx . ."] {
            assert_eq!(symbol_origin(symbol), "unknown", "{symbol:?}");
        }
    }

    #[test]
    fn a_relative_compdb_path_joins_the_project_root() {
        let mut resolver = ClangScipResolver::new(Some(PathBuf::from("build/compile_commands.json")));
        // Nothing at that path in a temp dir: prepare must not panic, and the
        // honest answer is `unavailable`, the same as a repository that never
        // generated a compilation database at all.
        let dir = std::env::temp_dir().join("callix-clang-resolver-test-relative");
        let _ = std::fs::create_dir_all(&dir);
        resolver.prepare(dir.clone(), None);
        assert_eq!(resolver.status(), ResolverStatus::Unavailable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absolute_compdb_path_is_not_rejoined_to_the_root() {
        let missing = std::env::temp_dir().join("callix-clang-resolver-does-not-exist.json");
        let mut resolver = ClangScipResolver::new(Some(missing));
        let dir = std::env::temp_dir().join("callix-clang-resolver-test-absolute");
        let _ = std::fs::create_dir_all(&dir);
        resolver.prepare(dir.clone(), None);
        assert_eq!(resolver.status(), ResolverStatus::Unavailable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_before_prepare_answers_nothing() {
        let resolver = ClangScipResolver::new(None);
        assert!(resolver.resolve_one(Path::new("a.cc"), 1, 1).is_none());
    }
}
