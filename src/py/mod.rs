//! The seam between callix and PyO3.
//!
//! With the `python` feature (the default, and what maturin builds) this module
//! only re-exports PyO3, and the crate is the Python extension module.
//!
//! Without it, the same sources build against `native`, which supplies the few
//! PyO3 types the adapters touch — `Python`, `Py`, `Bound`, `PyDict`, `PyList`,
//! `PyErr` — as plain Rust, and the attribute macros come from `callix-compat`
//! as no-ops. The result links no Python at all; it is what the `callix`
//! command line runs.

#[cfg(feature = "python")]
mod real;
#[cfg(feature = "python")]
pub use real::*;

#[cfg(not(feature = "python"))]
mod native;
#[cfg(not(feature = "python"))]
pub use native::*;
