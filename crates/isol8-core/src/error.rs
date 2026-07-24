//! Typed error for the isol8 engine API.
//!
//! Engine modules (`profile`, `env`, `home`, `filter`, `resolve`, `backends`,
//! `sandbox`) return [`Result`]. The CLI layer keeps `anyhow` and upconverts these
//! for free because [`Error`](enum@Error) implements `std::error::Error`.
//!
//! The enum is deliberately compact: named variants only for what an embedder would
//! reasonably match on, and a [`Error::Message`] catch-all for the long tail of
//! contextual messages (kept human-readable via [`ResultExt::ctx`]).

use thiserror::Error;

/// The typed error returned by the isol8 engine.
#[derive(Debug, Error)]
pub enum Error {
    /// `cmd[0]` could not be resolved to an executable on the host `PATH`.
    #[error("command {0:?} not found")]
    CommandNotFound(String),

    /// A `--set-env` / env override was malformed (e.g. missing `=`).
    #[error("invalid environment variable: {0}")]
    InvalidEnv(String),

    /// isol8 is already running inside an isol8 sandbox (nesting is unsupported).
    #[error(
        "isol8 is already running inside an isol8 sandbox — nested sandboxing is not \
         supported (macOS Seatbelt cannot nest). Run the command directly instead of \
         wrapping it in isol8 again."
    )]
    NestedSandbox,

    /// No sandbox backend exists for the current OS at runtime.
    #[error("unsupported OS: {0}")]
    UnsupportedOs(&'static str),

    /// The OS rejected the generated policy (e.g. macOS `sandbox-exec` exit 65).
    #[error("{0}")]
    PolicyRejected(String),

    /// Profile load / merge / `requires` resolution failure.
    #[error("{0}")]
    Profile(String),

    /// An underlying I/O error (filesystem, process spawn, …).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A TOML deserialization error (profile/config parsing).
    #[error(transparent)]
    Toml(#[from] toml::de::Error),

    /// Catch-all for the long tail of contextual failures.
    #[error("{0}")]
    Message(String),
}

/// Convenience alias for a `Result` whose error is [`Error`](enum@Error).
pub type Result<T> = std::result::Result<T, Error>;

/// Attach a human-readable context message to an error, mirroring
/// `anyhow::Context::with_context`. The original error is appended as `: {err}`.
pub trait ResultExt<T> {
    /// Wrap the error in [`Error::Message`], prefixed with the message from `f`.
    fn ctx<C: std::fmt::Display>(self, f: impl FnOnce() -> C) -> Result<T>;
}

impl<T, E: std::fmt::Display> ResultExt<T> for std::result::Result<T, E> {
    fn ctx<C: std::fmt::Display>(self, f: impl FnOnce() -> C) -> Result<T> {
        self.map_err(|e| Error::Message(format!("{}: {e}", f())))
    }
}
