//! Поиск корней языковых проектов в монорепозитории и сбор исходников.
//!
//! Общее правило: маркер в самом `search_root` НЕ скрывает вложенные
//! корни — иначе монорепозиторий, который сам является проектом, съел бы
//! свои подпроекты.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// Каталоги, которые не являются исходниками ни для одного языка.
pub const EXCLUDED_DIRS: [&str; 8] = [
    ".venv",
    "venv",
    "__pycache__",
    ".git",
    "dist",
    "build",
    ".eggs",
    "node_modules",
];

/// Лежит ли путь внутри исключённого каталога (относительно корня).
fn in_excluded_dir(path: &Path, search_root: &Path, excluded: &[&str]) -> bool {
    let Ok(relative) = path.strip_prefix(search_root) else {
        return false;
    };
    relative
        .components()
        .any(|c| excluded.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

/// Сортировка путей — как `sorted()` над `pathlib.Path` в Python.
///
/// Python сравнивает пути ПОКОМПОНЕНТНО, а не строкой целиком, и на этом
/// расходятся соседи вроде `src/v4-mini/...` и `src/v4/...`: строкой
/// первым идёт `v4-mini` (дефис < слэша), покомпонентно — `v4`
/// (короткий компонент — префикс длинного).
fn path_sort_key(path: &Path) -> Vec<String> {
    path.to_string_lossy()
        .split('/')
        .map(str::to_string)
        .collect()
}

pub fn sort_paths_like_python(paths: &mut [PathBuf]) {
    sort_paths(paths);
}

fn sort_paths(paths: &mut [PathBuf]) {
    paths.sort_by_key(|p| path_sort_key(p));
}

pub fn is_relative_to(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root).is_ok()
}

/// Все каталоги с валидными маркерами проекта.
///
/// `marker_filter` отсеивает ложные срабатывания (например, pyproject.toml
/// без секции `[project]`). Пустой результат откатывается к самому
/// `search_root`.
pub fn collect_marker_roots(
    search_root: &Path,
    markers: &[&str],
    excluded: &[&str],
    marker_filter: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for entry in WalkDir::new(search_root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !markers.contains(&name.as_ref()) {
            continue;
        }
        if in_excluded_dir(path, search_root, excluded) || !marker_filter(path) {
            continue;
        }
        let Some(root) = path.parent() else { continue };
        if seen.insert(root.to_path_buf()) {
            roots.push(root.to_path_buf());
        }
    }

    if roots.is_empty() {
        return vec![search_root.to_path_buf()];
    }
    sort_paths(&mut roots);
    roots
}

/// Отбрасывает файлы, принадлежащие вложенным корням: родительский корень
/// не должен разбирать файлы своих подпроектов.
pub fn filter_nested_root_files(
    files: Vec<PathBuf>,
    current_root: &Path,
    project_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let nested: Vec<&PathBuf> = project_roots
        .iter()
        .filter(|root| root.as_path() != current_root && is_relative_to(root, current_root))
        .collect();

    files
        .into_iter()
        .filter(|file| !nested.iter().any(|root| is_relative_to(file, root)))
        .collect()
}

/// Все файлы с указанными расширениями под корнем, кроме служебных каталогов.
pub fn collect_files(root: &Path, extensions: &[&str], excluded: &[&str]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            let matches_ext = p
                .extension()
                .map(|e| {
                    let dotted = format!(".{}", e.to_string_lossy());
                    extensions.contains(&dotted.as_str())
                })
                .unwrap_or(false);
            matches_ext && !in_excluded_dir(p, root, excluded)
        })
        .collect();
    sort_paths(&mut files);
    files
}

/// Есть ли под корнем хоть один файл с таким расширением.
pub fn any_file_with_extension(root: &Path, extension: &str) -> bool {
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .any(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|ext| ext == extension)
        })
}

/// Значения ключа по секциям INI-файла.
///
/// Понимает продолжения строк с отступом — так `install_requires` в
/// setup.cfg почти всегда и записан:
///
/// ```ini
/// [options]
/// install_requires =
///     requests
///     httpx
/// ```
///
/// Ключи сравниваются без учёта регистра (как `optionxform` в
/// configparser), имена секций — с учётом.
pub fn ini_sections(text: &str, key: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut section = String::new();
    let mut collecting: Option<Vec<String>> = None;

    let flush = |out: &mut Vec<(String, Vec<String>)>,
                 section: &str,
                 collected: Option<Vec<String>>| {
        if let Some(values) = collected {
            out.push((section.to_string(), values));
        }
    };

    for raw in text.lines() {
        let trimmed = raw.trim();
        let indented = raw.starts_with([' ', '\t']);

        // Продолжение значения — строка с отступом, пока не начнётся новое.
        if collecting.is_some() && indented && !trimmed.is_empty() && !trimmed.starts_with('[') {
            if !trimmed.starts_with('#') && !trimmed.starts_with(';') {
                collecting.as_mut().expect("проверено выше").push(trimmed.to_string());
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        if let Some(header) = trimmed.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            flush(&mut out, &section, collecting.take());
            section = header.trim().to_string();
            continue;
        }

        if let Some((name, value)) = trimmed.split_once('=') {
            flush(&mut out, &section, collecting.take());
            if name.trim().eq_ignore_ascii_case(key) {
                let value = value.trim();
                collecting = Some(if value.is_empty() {
                    Vec::new()
                } else {
                    vec![value.to_string()]
                });
            }
        }
    }
    flush(&mut out, &section, collecting);
    out
}
