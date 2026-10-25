//! What a command can fail with, and the exit code that goes with it.

use std::fmt;

/// Exit codes. Usage errors are `2`, which clap picks itself.
pub mod code {
    /// Analysis, parsing or I/O failed.
    pub const FAILED: u8 = 1;
    /// `--strict` refused a graph it could not vouch for.
    pub const STRICT: u8 = 3;
}

#[derive(Debug)]
pub struct Failure {
    pub code: u8,
    pub message: String,
}

impl Failure {
    pub fn new(message: impl Into<String>) -> Self {
        Self { code: code::FAILED, message: message.into() }
    }

    /// Exit with `code` and nothing on stderr: the caller has said it all.
    pub fn silent(code: u8) -> Self {
        Self { code, message: String::new() }
    }

    pub fn strict(message: impl Into<String>) -> Self {
        Self { code: code::STRICT, message: message.into() }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<std::io::Error> for Failure {
    fn from(err: std::io::Error) -> Self {
        Self::new(err.to_string())
    }
}

impl From<callix_export::Error> for Failure {
    fn from(err: callix_export::Error) -> Self {
        Self::new(err.to_string())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(err: serde_json::Error) -> Self {
        Self::new(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Failure>;
