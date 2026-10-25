use crate::py::PyErr;
use crate::py::exceptions::PyOSError;

#[cfg(feature = "python")]
mod classes {
    use crate::py::exceptions::PyException;
    use pyo3_stub_gen::create_exception;

    // The module is passed as tokens (not a string) — stringify! yields
    // "callix._core".
    create_exception!(callix._core, CallixError, PyException, "Base callix error.");
    create_exception!(callix._core, ParseError, CallixError, "Could not parse the source.");
    create_exception!(callix._core, ResolverTimeout, ParseError, "The resolver exceeded its timeout.");
    create_exception!(callix._core, DuplicateNodeError, CallixError, "A node with this ID is already in the graph.");
    create_exception!(callix._core, SerializationError, CallixError, "Graph (de)serialization error.");
    create_exception!(callix._core, AdapterError, CallixError, "Error raised while the adapter was running.");
}

/// Without Python the exception classes are names a `PyErr` carries.
#[cfg(not(feature = "python"))]
mod classes {
    use crate::py::PyErr;

    macro_rules! error_class {
        ($($name:ident),* $(,)?) => {$(
            pub struct $name;

            impl $name {
                pub fn new_err(message: impl Into<String>) -> PyErr {
                    PyErr::new(stringify!($name), message)
                }
            }
        )*};
    }

    error_class!(
        CallixError,
        ParseError,
        ResolverTimeout,
        DuplicateNodeError,
        SerializationError,
        AdapterError,
    );
}

pub use classes::{
    AdapterError, CallixError, DuplicateNodeError, ParseError, ResolverTimeout, SerializationError,
};

#[derive(thiserror::Error, Debug)]
pub enum CoreError {
    #[error("failed to parse {path}: {reason}")]
    Parse { path: String, reason: String },
    #[error("resolver timed out after {0}ms")]
    Timeout(u64),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<CoreError> for PyErr {
    fn from(e: CoreError) -> PyErr {
        match e {
            CoreError::Parse { .. } => ParseError::new_err(e.to_string()),
            CoreError::Timeout(_) => ResolverTimeout::new_err(e.to_string()),
            CoreError::Io(io) => PyOSError::new_err(io.to_string()),
        }
    }
}
