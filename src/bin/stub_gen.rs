//! Генератор .pyi. Запускается из dev.sh: `cargo run --bin stub_gen`.
//! Пути и имя модуля берутся из pyproject.toml, файлы кладутся рядом с python-source.

use std::fs;
use std::path::Path;

const STUB: &str = "python/callix/_core/__init__.pyi";

/// Константы модуля, объявленные через `m.add(...)`: pyo3-stub-gen их не
/// видит, поэтому дописываем руками.
const CONSTANTS: &str = "\n\
__version__: builtins.str\n\
SCHEMA_VERSION: builtins.int\n\
RESOLVER_STATUS_KEY: builtins.str\n\
RESOLVER_METRICS_KEY: builtins.str\n";

fn main() -> pyo3_stub_gen::Result<()> {
    callix::stub_info()?.generate()?;

    let path = Path::new(STUB);
    if path.exists() {
        let mut src = fs::read_to_string(path)?;
        // pyo3-stub-gen 0.23 приписывает базовому классу исключения префикс
        // `builtins.`, из-за чего `ResolverTimeout(builtins.ParseError)` не
        // резолвится тайпчекерами.
        src = src.replace("builtins.ParseError", "ParseError");
        src = src.replace("builtins.CallixError", "CallixError");
        if !src.contains("__version__:") {
            src.push_str(CONSTANTS);
        }
        fs::write(path, src)?;
    }

    println!("stubs -> {STUB}");
    Ok(())
}
