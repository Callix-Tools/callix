//! PyO3 itself, under the names the rest of the crate imports.

pub use pyo3::PyErr;

pub mod prelude {
    pub use pyo3::prelude::*;
}

pub mod types {
    pub use pyo3::types::*;
}

pub mod exceptions {
    pub use pyo3::exceptions::*;
}

pub mod derive {
    pub use pyo3_stub_gen::derive::*;
}
