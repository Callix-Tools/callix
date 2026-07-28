//! A module's qualified name from its file path, source roots, and tsconfig
//! aliases.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;

/// Extensions stripped when turning a path into a module name.
const TS_EXTENSIONS: [&str; 4] = ["ts", "tsx", "mts", "cts"];

/// Files that represent the package itself — Python's `__init__.py` analogue.
const INDEX_STEMS: [&str; 1] = ["index"];

/// Source roots: `src/` when files live there, plus the root itself for
/// everything outside `src/`.
pub fn source_roots(project_root: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let src = project_root.join("src");
    if src.is_dir() && !files.is_empty() && files.iter().any(|f| f.starts_with(&src)) {
        return vec![src, project_root.to_path_buf()];
    }
    vec![project_root.to_path_buf()]
}

/// Strips `//` comments and trailing commas — tsconfig.json is usually JSONC
/// rather than strict JSON.
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

    // A trailing comma before } or ]
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

/// Path aliases from `compilerOptions.paths` of the form
/// `"<prefix>/*": ["<target>/*"]`.
///
/// Entries with multiple targets or without a glob are ignored. Read and
/// parse errors yield an empty map — this never fails.
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

/// Rewrites an import when it starts with a tsconfig alias.
///
/// With `{"@/": "src/"}` the path `@/client/v2` becomes `src/client/v2`.
pub fn apply_alias(import_path: &str, aliases: &BTreeMap<String, String>) -> String {
    for (prefix, target) in aliases {
        if let Some(rest) = import_path.strip_prefix(prefix.as_str()) {
            return format!("{target}{rest}");
        }
    }
    import_path.to_string()
}

/// File path → dotted module name.
///
/// `src/pkg/index.ts` → `pkg`, `src/pkg/utils.ts` → `pkg.utils`,
/// `src/pkg/ui.tsx` → `pkg.ui`. Declaration files (`.d.ts`) map the same
/// way — they are filtered out one level up.
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
        // For declarations drop the inner `.d`: foo.d.ts → foo
        if let Some(without) = stem.strip_suffix(".d") {
            stem = without.to_string();
        }
    }
    *last = stem;

    // index files represent the package itself
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

/// A relative import → an absolute module name.
///
/// `("pkg.core", "./utils")` → `pkg.utils`; `("pkg.core", "../shared")` →
/// `shared`; `("pkg.core", ".")` → `pkg`.
pub fn resolve_relative(current_module_qname: &str, import_path: &str) -> String {
    let current_parts: Vec<&str> = current_module_qname.split('.').collect();
    // Start from the current file's directory, dropping the module name.
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
        // Walked above the root — take the top segment of the original name.
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
