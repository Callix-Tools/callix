//! Полное имя модуля по пути файла, корни исходников и алиасы tsconfig.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

/// Расширения, которые срезаются при переводе пути в имя модуля.
const TS_EXTENSIONS: [&str; 4] = ["ts", "tsx", "mts", "cts"];

/// Файлы, представляющие сам пакет — аналог `__init__.py` в Python.
const INDEX_STEMS: [&str; 1] = ["index"];

/// Корни исходников: `src/`, если файлы лежат там, плюс сам корень для
/// всего, что вне `src/`.
pub fn source_roots(project_root: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let src = project_root.join("src");
    if src.is_dir() && !files.is_empty() && files.iter().any(|f| f.starts_with(&src)) {
        return vec![src, project_root.to_path_buf()];
    }
    vec![project_root.to_path_buf()]
}

/// Убирает `//`-комментарии и висящие запятые — tsconfig.json обычно
/// JSONC, а не строгий JSON.
fn strip_jsonc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for line in raw.lines() {
        let mut in_string = false;
        let mut escaped = false;
        let mut cut = line.len();
        let bytes = line.as_bytes();
        for i in 0..bytes.len() {
            let ch = bytes[i] as char;
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' if in_string => escaped = true,
                '"' => in_string = !in_string,
                '/' if !in_string && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                    cut = i;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }

    // Висящая запятая перед } или ]
    let mut cleaned = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
                continue;
            }
        }
        cleaned.push(chars[i]);
        i += 1;
    }
    cleaned
}

/// Алиасы путей из `compilerOptions.paths` вида `"<prefix>/*": ["<target>/*"]`.
///
/// Записи с несколькими целями и без glob игнорируются. Ошибки чтения и
/// разбора дают пустую карту — метод не падает никогда.
pub fn path_aliases(project_root: &Path) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    let Ok(raw) = fs::read_to_string(project_root.join("tsconfig.json")) else {
        return aliases;
    };
    let Ok(data) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&raw)) else {
        return aliases;
    };
    let Some(paths) = data
        .get("compilerOptions")
        .and_then(|c| c.get("paths"))
        .and_then(|p| p.as_object())
    else {
        return aliases;
    };

    for (pattern, targets) in paths {
        if !pattern.ends_with("/*") {
            continue;
        }
        let Some(targets) = targets.as_array() else {
            continue;
        };
        if targets.len() != 1 {
            continue;
        }
        let Some(target) = targets[0].as_str() else {
            continue;
        };
        if !target.ends_with("/*") {
            continue;
        }
        let alias_prefix = &pattern[..pattern.len() - 1];
        let target_prefix = target.trim_start_matches("./");
        let target_prefix = &target_prefix[..target_prefix.len() - 1];
        aliases.insert(alias_prefix.to_string(), target_prefix.to_string());
    }
    aliases
}

/// Переписывает импорт, если он начинается с алиаса tsconfig.
///
/// С `{"@/": "src/"}` путь `@/client/v2` становится `src/client/v2`.
pub fn apply_alias(import_path: &str, aliases: &BTreeMap<String, String>) -> String {
    for (prefix, target) in aliases {
        if let Some(rest) = import_path.strip_prefix(prefix.as_str()) {
            return format!("{target}{rest}");
        }
    }
    import_path.to_string()
}

/// Путь файла → точечное имя модуля.
///
/// `src/pkg/index.ts` → `pkg`, `src/pkg/utils.ts` → `pkg.utils`,
/// `src/pkg/ui.tsx` → `pkg.ui`. Файлы деклараций (`.d.ts`) отображаются
/// так же — отсекаются они уровнем выше.
pub fn qualified_name(file_path: &Path, source_root: &Path) -> Option<String> {
    let relative = file_path.strip_prefix(source_root).ok()?;
    let mut parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();

    let last = parts.last_mut()?;
    let as_path = Path::new(last.as_str());
    let suffix = as_path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut stem = as_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if TS_EXTENSIONS.contains(&suffix.as_str()) {
        // Для деклараций снимаем внутренний `.d`: foo.d.ts → foo
        if let Some(without) = stem.strip_suffix(".d") {
            stem = without.to_string();
        }
    }
    *last = stem;

    // index-файлы представляют сам пакет
    if parts
        .last()
        .is_some_and(|p| INDEX_STEMS.contains(&p.as_str()))
    {
        parts.pop();
    }

    if parts.is_empty() {
        return source_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
    }
    Some(parts.join("."))
}

/// Относительный импорт → абсолютное имя модуля.
///
/// `("pkg.core", "./utils")` → `pkg.utils`; `("pkg.core", "../shared")` →
/// `shared`; `("pkg.core", ".")` → `pkg`.
pub fn resolve_relative(current_module_qname: &str, import_path: &str) -> String {
    let current_parts: Vec<&str> = current_module_qname.split('.').collect();
    // Стартуем от каталога текущего файла, отбросив имя модуля.
    let mut base: Vec<String> = if current_parts.len() > 1 {
        current_parts[..current_parts.len() - 1]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        Vec::new()
    };

    for segment in import_path.replace('\\', "/").split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                base.pop();
            }
            _ => {
                let stem = match segment.split_once('.') {
                    Some((before, _)) => before,
                    None => segment,
                };
                if !stem.is_empty() && !INDEX_STEMS.contains(&stem) {
                    base.push(stem.to_string());
                }
            }
        }
    }

    if base.is_empty() {
        // Ушли выше корня — берём верхний сегмент исходного имени.
        return current_parts[0].to_string();
    }
    base.join(".")
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn ts_file_to_qualified_name(file_path: PathBuf, source_root: PathBuf) -> PyResult<String> {
    qualified_name(&file_path, &source_root).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "{file_path:?} is not in the subpath of {source_root:?}"
        ))
    })
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn ts_resolve_relative_import(current_module_qname: &str, import_path: &str) -> String {
    resolve_relative(current_module_qname, import_path)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn ts_load_path_aliases(project_root: PathBuf) -> BTreeMap<String, String> {
    path_aliases(&project_root)
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn ts_find_source_roots(project_root: PathBuf, files: Vec<PathBuf>) -> Vec<PathBuf> {
    source_roots(&project_root, &files)
}
