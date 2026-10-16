//! The crate's error type.

use std::path::PathBuf;

use crate::diagnostics::Diagnostic;

/// Everything that can stop generation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A schema document named by the caller or by a `$ref` does not exist.
    #[error("schema not found: {}", .0.display())]
    SchemaNotFound(PathBuf),

    /// The configuration file is malformed or contradicts itself.
    #[error("invalid configuration: {0}")]
    Config(String),

    /// A Rust type written as text (in config or a policy) cannot be modelled.
    #[error("invalid Rust type `{input}`: {reason}")]
    InvalidType {
        /// The offending text.
        input: String,
        /// Why it was rejected.
        reason: String,
    },

    /// A name derived from a schema is not a legal Rust identifier.
    #[error("{context}: `{name}` is not a valid Rust identifier")]
    InvalidIdentifier {
        /// The text that was meant to be an identifier.
        name: String,
        /// What the identifier was for.
        context: String,
    },

    /// Resolution produced errors; every one is listed.
    #[error("{}", render_diagnostics(.0))]
    Diagnostics(Vec<Diagnostic>),

    /// The assembled output is not valid Rust.
    #[error("generated Rust is invalid: {0}")]
    InvalidOutput(#[from] syn::Error),

    /// The model holds something that has no Rust rendering.
    #[error("cannot render: {0}")]
    Render(String),

    /// A file could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying failure.
        source: std::io::Error,
    },

    /// A generated file differs from the one on disk (`--check`).
    #[error("{} is out of date; regenerate it", .0.display())]
    OutOfDate(PathBuf),
}

impl Error {
    pub(crate) fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }

    pub(crate) fn render(message: impl Into<String>) -> Self {
        Self::Render(message.into())
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

fn render_diagnostics(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The crate's result type.
pub type Result<T, E = Error> = std::result::Result<T, E>;
