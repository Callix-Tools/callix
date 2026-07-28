//! Резолвер символов Rust поверх пакетного индекса `rust-analyzer scip`.
//!
//! Интерактивный LSP-сервер держит состояние анализа всего воркспейса в
//! памяти и на больших проектах разрастается до десятков гигабайт. Индекс
//! SCIP пишется один раз, читается статически и отвечает на запросы из
//! таблиц в памяти — поэтому здесь именно он, а не LSP.
//!
//! Как и Go, вкомпилировать сюда язык целиком нельзя: нужен установленный
//! `rust-analyzer` (и Cargo) — но для Rust-проекта они и так есть.

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

/// Потолок на один прогон `rust-analyzer scip`. Большие воркспейсы
/// укладываются сильно раньше; ограничение страхует от зависания.
const SCIP_TIMEOUT: Duration = Duration::from_secs(1800);

/// Крейты стандартной поставки Rust в символе SCIP. Символ cargo выглядит
/// как `<scheme> cargo <package> <version> <descriptors>`.
const STD_PACKAGES: [&str; 5] = ["std", "core", "alloc", "proc_macro", "test"];

/// Символу cargo нужны хотя бы `<scheme> <manager> <package> <version>`.
const SCIP_SYMBOL_MIN_PARTS: usize = 4;

/// Происхождение внешнего символа читается прямо из его схемы.
///
/// Это надёжнее, чем гадать по пути файла с определением, и работает даже
/// когда исходники крейта вообще не открывались.
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

/// Первый исполняемый файл с таким именем в PATH.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Конкретный путь к rust-analyzer, который rustup выдаёт из `cwd`.
fn rustup_which(rustup: &Path, cwd: &Path) -> Option<PathBuf> {
    let output = Command::new(rustup)
        .args(["which", "rust-analyzer"])
        .current_dir(cwd)
        // Пин из окружения перебил бы rust-toolchain.toml проекта.
        .env_remove("RUSTUP_TOOLCHAIN")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    path.is_file().then_some(path)
}

/// Какой именно rust-analyzer запускать для этого проекта.
///
/// `rust-analyzer` в PATH обычно не бинарь, а прокси rustup, уважающий
/// `rust-toolchain.toml` проекта. Отсюда два случая:
///
/// 1. В закреплённом тулчейне компонент есть — нужен именно он: сборка под
///    тулчейн проекта разбирает его корректно. Поэтому сначала спрашиваем
///    `rustup which` из корня проекта.
/// 2. В закреплённом тулчейне компонента нет (так, ruff пинит 1.96 без
///    rust-analyzer) — прокси вышел бы с `Unknown binary`, и резолв молча
///    дал бы пустоту. Тогда откатываемся к тулчейну по умолчанию, спросив
///    из нейтрального каталога.
fn resolve_ra_binary(project_root: &Path) -> PathBuf {
    let fallback = || which("rust-analyzer").unwrap_or_else(|| PathBuf::from("rust-analyzer"));
    match which("rustup") {
        Some(rustup) => rustup_which(&rustup, project_root)
            .or_else(|| rustup_which(&rustup, &std::env::temp_dir()))
            .unwrap_or_else(fallback),
        None => fallback(),
    }
}

/// Ждёт завершения процесса, убивая его по таймауту.
///
/// Возвращает код возврата (None, если процесс пришлось убить).
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

/// Позиция определения: индекс документа и 0-based координаты.
type Location = (u32, u32, u32);

#[gen_stub_pyclass]
#[pyclass(module = "callix._core")]
pub struct RustResolver {
    root: Option<PathBuf>,
    status: ResolverStatus,
    /// Относительные пути документов индекса и обратный поиск по ним.
    docs: Vec<String>,
    doc_index: HashMap<String, u32>,
    /// Пул символов: одна строка на много вхождений.
    symbols: Vec<String>,
    symbol_index: HashMap<String, u32>,
    /// Документ → `(строка, колонка)` → символ. Координаты 0-based.
    by_doc: Vec<HashMap<(u32, u32), u32>>,
    /// Глобальный символ → место его определения.
    defs: HashMap<u32, Location>,
    /// Документ → символ `local …` → место определения внутри документа.
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

    /// Сворачивает индекс SCIP в таблицы поиска.
    fn ingest(&mut self, data: &[u8]) {
        for_each_document(data, |relative_path, occurrences| {
            let mut doc: Option<u32> = None;
            for occurrence in occurrences {
                if occurrence.symbol.is_empty() {
                    continue;
                }
                // Документ заводится только когда в нём есть что искать.
                let doc = *doc.get_or_insert_with(|| self.intern_doc(&relative_path));
                let is_local = occurrence.symbol.starts_with("local ");
                let index = self.intern_symbol(&occurrence.symbol);
                let position = (occurrence.start_line, occurrence.start_col);
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

    /// Запускает `rust-analyzer scip`; отдаёт `(байты индекса, код возврата)`.
    fn run_scip(&self, project_root: &Path) -> (Option<Vec<u8>>, Option<i32>) {
        let binary = resolve_ra_binary(project_root);
        // temp_dir уважает TMPDIR: индекс большого воркспейса весит сотни
        // мегабайт, и на маленьком /tmp его стоит увести в другое место.
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
        let root = self.root.as_ref().expect("root задан при prepare");
        ResolvedRef {
            full_name: String::new(),
            file_path: Some(root.join(&self.docs[doc as usize]).to_string_lossy().into_owned()),
            line: line + 1,
            col: col + 1,
            kind: String::new(),
            origin: origin.to_string(),
        }
    }

    /// Символ → его определение либо внешняя ссылка.
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

    /// Абсолютный путь → путь относительно корня индекса.
    ///
    /// В SCIP `relative_path` всегда со слэшами, поэтому и здесь только они.
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

    /// Строит индекс SCIP для воркспейса.
    ///
    /// Список файлов не нужен: `rust-analyzer` сам разбирает `Cargo.toml` и
    /// индексирует весь воркспейс.
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
            // rust-analyzer упал посреди прогона (например, крейт не
            // загрузился), но оставил частичный индекс: помечаем degraded,
            // чтобы strict-режим не принял неполный граф за полный.
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
