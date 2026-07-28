//! The .pyi generator, run by `task stubs`: `cargo run --bin stub_gen`.
//! Paths and the module name come from pyproject.toml; files are written
//! next to python-source.

use std::fs;
use std::path::Path;

const STUB: &str = "python/callix/_core/__init__.pyi";

/// Module constants declared via `m.add(...)`: pyo3-stub-gen does not see
/// them, so they are appended by hand.
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
        // pyo3-stub-gen 0.23 prefixes an exception's base class with
        // `builtins.`, which leaves `ResolverTimeout(builtins.ParseError)`
        // unresolvable for type checkers.
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
