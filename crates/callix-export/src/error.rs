//! The one error type of the crate.

use std::io;

/// Everything that can go wrong reading a graph or writing a report.
///
/// As with the Python `SerializationError`, every way a payload can be wrong
/// funnels into one variant: [`Error::Invalid`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid graph: {0}")]
    Invalid(String),
    #[error("unsupported schema_version {found} (this build reads {expected})")]
    Schema { found: u64, expected: u32 },
    #[error("unknown format {0:?}")]
    UnknownFormat(String),
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::Invalid(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
