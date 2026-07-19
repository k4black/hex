//! One error type for the runtime's fallible IO and parsing paths.

use std::fmt;

/// A runtime error with a human-readable message. Kept deliberately simple —
/// the runtime maps these to stable CLI exit codes and stderr diagnostics.
#[derive(Debug)]
pub struct HexError(pub String);

impl HexError {
    /// Build an error from any displayable message.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for HexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HexError {}

impl From<std::io::Error> for HexError {
    fn from(e: std::io::Error) -> Self {
        Self(format!("io: {e}"))
    }
}

impl From<serde_yaml::Error> for HexError {
    fn from(e: serde_yaml::Error) -> Self {
        Self(format!("yaml: {e}"))
    }
}

impl From<serde_json::Error> for HexError {
    fn from(e: serde_json::Error) -> Self {
        Self(format!("json: {e}"))
    }
}

/// Convenience alias for runtime results.
pub type Result<T> = std::result::Result<T, HexError>;
