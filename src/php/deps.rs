//! Composer dependencies and import classification.
//!
//! PHP has a wrinkle the other four languages do not: a Composer package name
//! is not the namespace that `use` statements reference. Nothing in the
//! manifest maps `symfony/console` to `Symfony\Component\Console` — only the
//! installed `vendor/` tree knows that.
//!
//! So classification matches **vendor prefixes**: the segment before the
//! slash, lowercased, against the lowercased first segment of the imported
//! namespace. That gets `Symfony` ↔ `symfony/*`, `Monolog` ↔
//! `monolog/monolog` and `Psr` ↔ `psr/log` from the manifest alone, which is
//! the overwhelming majority of real imports. It is a heuristic, and callix
//! reports the ones it cannot place as `unknown` rather than guessing.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use serde_json::Value;

use super::detector::composer;

/// PHP has no importable standard library: built-in functions are global and
/// built-in classes live in the global namespace. A `use` of one of these is a
/// single unqualified segment — `use DateTime;` — which is the signal.
const BUILTIN_CLASSES: [&str; 99] = [
    // Core and SPL
    "stdClass",
    "Closure",
    "Generator",
    "WeakMap",
    "WeakReference",
    "ArrayObject",
    "ArrayIterator",
    "ArrayAccess",
    "Countable",
    "Iterator",
    "IteratorAggregate",
    "Traversable",
    "Stringable",
    "JsonSerializable",
    "Serializable",
    "UnitEnum",
    "BackedEnum",
    "SplStack",
    "SplQueue",
    "SplDoublyLinkedList",
    "SplFixedArray",
    "SplObjectStorage",
    "SplPriorityQueue",
    "SplHeap",
    "SplMinHeap",
    "SplMaxHeap",
    "SplFileObject",
    "SplFileInfo",
    "SplTempFileObject",
    "DirectoryIterator",
    "RecursiveIteratorIterator",
    "RecursiveDirectoryIterator",
    "FilesystemIterator",
    // Throwables
    "Throwable",
    "Exception",
    "Error",
    "TypeError",
    "ValueError",
    "ArgumentCountError",
    "ArithmeticError",
    "DivisionByZeroError",
    "RuntimeException",
    "LogicException",
    "InvalidArgumentException",
    "OutOfRangeException",
    "OutOfBoundsException",
    "LengthException",
    "DomainException",
    "RangeException",
    "UnexpectedValueException",
    "UnderflowException",
    "OverflowException",
    "BadFunctionCallException",
    "BadMethodCallException",
    "JsonException",
    // Date and time
    "DateTime",
    "DateTimeImmutable",
    "DateTimeInterface",
    "DateInterval",
    "DateTimeZone",
    "DatePeriod",
    // Reflection
    "ReflectionClass",
    "ReflectionObject",
    "ReflectionMethod",
    "ReflectionFunction",
    "ReflectionProperty",
    "ReflectionParameter",
    "ReflectionNamedType",
    "ReflectionEnum",
    "ReflectionAttribute",
    "ReflectionException",
    "Attribute",
    // Extensions bundled with PHP
    "PDO",
    "PDOStatement",
    "PDOException",
    "mysqli",
    "SQLite3",
    "DOMDocument",
    "DOMElement",
    "DOMNode",
    "SimpleXMLElement",
    "XMLReader",
    "XMLWriter",
    "ZipArchive",
    "finfo",
    "IntlDateFormatter",
    "NumberFormatter",
    "Collator",
    "Locale",
    "CURLFile",
    "GMP",
    // Fibers and enums added in 8.x
    "Fiber",
    "FiberError",
    "SensitiveParameter",
    "ReturnTypeWillChange",
    "AllowDynamicProperties",
    "Override",
    "Deprecated",
    "NoDiscard",
];

/// Platform requirements are not packages.
fn is_platform_requirement(package: &str) -> bool {
    package == "php"
        || package.starts_with("ext-")
        || package.starts_with("lib-")
        || package.starts_with("php-")
}

/// The lowercased vendor segment of a `vendor/package` name.
fn vendor_prefix(package: &str) -> Option<String> {
    let (vendor, _) = package.split_once('/')?;
    let vendor = vendor.trim().to_lowercase();
    (!vendor.is_empty()).then_some(vendor)
}

/// Vendor prefixes from composer.json's `require` and `require-dev`.
///
/// Dev requirements are included so a test-only package classifies as
/// `third_party` rather than `unknown`.
fn manifest_vendors(root: &Path) -> HashSet<String> {
    let mut vendors = HashSet::new();
    let composer = composer(root);

    for section in ["require", "require-dev"] {
        let Some(Value::Object(block)) = composer.get(section) else {
            continue;
        };
        for package in block.keys() {
            if is_platform_requirement(package) {
                continue;
            }
            if let Some(vendor) = vendor_prefix(package) {
                vendors.insert(vendor);
            }
        }
    }
    vendors
}

/// Vendor prefixes from composer.lock's `packages` and `packages-dev`.
///
/// The lock file also names transitive packages, so an import of something
/// pulled in indirectly still classifies as third-party rather than unknown.
fn lock_vendors(root: &Path) -> HashSet<String> {
    let mut vendors = HashSet::new();
    let Ok(text) = fs::read_to_string(root.join("composer.lock")) else {
        return vendors;
    };
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&text) else {
        return vendors;
    };

    for section in ["packages", "packages-dev"] {
        let Some(Value::Array(block)) = data.get(section) else {
            continue;
        };
        for entry in block {
            if let Some(Value::String(name)) = entry.get("name")
                && let Some(vendor) = vendor_prefix(name)
            {
                vendors.insert(vendor);
            }
        }
    }
    vendors
}

/// Every declared dependency of the project, as vendor prefixes.
pub fn dependencies(root: &Path) -> HashSet<String> {
    let mut vendors = manifest_vendors(root);
    vendors.extend(lock_vendors(root));
    vendors
}

/// The namespace prefixes the project itself declares, as first segments.
///
/// `"App\\": "src/"` makes `App` internal; a class referenced as
/// `App\Domain\Engine` therefore does not look third-party.
pub fn internal_prefixes(root: &Path) -> HashSet<String> {
    super::detector::psr4_roots(root)
        .into_iter()
        .filter_map(|(prefix, _)| {
            prefix
                .trim_start_matches('\\')
                .split('\\')
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// PHP's built-in class names, treated as stdlib.
pub fn stdlib_names() -> HashSet<String> {
    BUILTIN_CLASSES.iter().map(|s| s.to_string()).collect()
}

/// Where an imported name comes from.
///
/// The comparison is case-insensitive on the vendor side because Composer
/// package names are lowercase by convention while namespaces are
/// StudlyCaps — `symfony/console` against `Symfony`.
pub fn classify_import(
    import_path: &str,
    internal: &HashSet<String>,
    vendors: &HashSet<String>,
) -> &'static str {
    let path = import_path.trim_start_matches('\\');
    let Some(top) = path.split('\\').next().filter(|s| !s.is_empty()) else {
        return "unknown";
    };

    // A single unqualified segment naming a built-in class: `use DateTime;`.
    if !path.contains('\\') && BUILTIN_CLASSES.contains(&top) {
        return "stdlib";
    }
    if internal.contains(top) {
        return "internal";
    }
    if vendors.contains(&top.to_lowercase()) {
        return "third_party";
    }
    "unknown"
}

#[gen_stub_pyfunction]
#[pyfunction]
pub fn php_parse_dependencies(root: PathBuf) -> Vec<String> {
    let mut names: Vec<String> = dependencies(&root).into_iter().collect();
    names.sort();
    names
}

#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (import_path, internal = None, vendors = None))]
pub fn classify_php_import(
    import_path: &str,
    internal: Option<HashSet<String>>,
    vendors: Option<HashSet<String>>,
) -> &'static str {
    classify_import(
        import_path,
        &internal.unwrap_or_default(),
        &vendors.unwrap_or_default(),
    )
}
