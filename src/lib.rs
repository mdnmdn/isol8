//! `isol8` — a lightweight, cross-platform isolation sandbox for AI coding agents and
//! CLI tools.
//!
//! It wraps an arbitrary command so it runs unprivileged with a **deny-by-default**,
//! restricted view of the filesystem, a sanitized environment, and a replaceable
//! `$HOME`. The same engine backs both the `isol8` binary and this library: macOS uses
//! Seatbelt (`sandbox-exec`), Linux uses Landlock, and Windows uses an AppContainer
//! (draft). See [`_docs/project-description.md`] for the full specification.
//!
//! # Embedding
//!
//! Use the [`Sandbox`] builder to confine a command from another Rust program:
//!
//! ```no_run
//! // Run a command confined, blocking until it exits.
//! let code: i32 = isol8::Sandbox::new()
//!     .profile("base")
//!     .grant_rw("/my/project")
//!     .home("/tmp/scratch")
//!     .run(["node", "script.js"])?;
//!
//! // Or launch it and keep a non-blocking handle.
//! let mut child = isol8::Sandbox::new().profile("base").spawn(["sleep", "5"])?;
//! let _ = child.id();
//! let code = child.wait()?;
//!
//! // Or resolve + render the effective policy without spawning.
//! let dry = isol8::Sandbox::new().profile("base").dry_run(["node", "x"])?;
//! println!("{}", dry.policy);
//! # Ok::<(), isol8::Error>(())
//! ```
//!
//! For engine-only embedding (no `clap`/`serde_yaml`), depend on isol8 with
//! `default-features = false`.
//!
//! # Module map
//!
//! - [`sandbox`] — the embedding entry surface: [`Spec`], [`Sandbox`], [`SandboxChild`],
//!   [`DryRun`].
//! - [`profile`] — the [`Profile`] model ([`PathGrant`]/[`Access`]/[`MatchKind`]),
//!   TOML loading, and deny-first merge. **Drives everything.**
//! - [`resolve`] — the shared [`effective_policy`] pipeline and [`confine_executable`].
//! - [`home`] / [`env`](mod@env) — `$HOME` resolution (R4) and sanitized environment (R3).
//! - [`filter`] — conditional layer/policy matching (OS / arch / executable).
//! - [`backends`] — the per-OS [`backends::Backend`] implementations.
//! - [`error`] — the typed [`Error`] and [`Result`] returned by the engine.
//!
//! [`_docs/project-description.md`]: https://github.com/eugene1g/agent-safehouse

#![warn(missing_docs)]

/// Per-OS sandbox [`backends::Backend`] implementations (Seatbelt / Landlock /
/// AppContainer) plus backend [`backends::select`]ion.
pub mod backends;
/// Sanitized environment construction (R3): minimal allowlist, HOME first.
pub mod env;
pub mod error;
pub mod filter;
pub mod home;
/// The [`Profile`] model: path grants, capabilities, TOML loading, deny-first merge.
pub mod profile;
pub mod resolve;
pub mod sandbox;

/// CLI surface (arg parsing, config, diag, the binary entry point). Behind the
/// default-on `cli` feature; not part of the stable embedding API.
#[cfg(feature = "cli")]
pub mod cli;

pub use error::{Error, Result};
pub use profile::{Access, MatchKind, PathGrant, Profile};
pub use resolve::{confine_executable, effective_policy, EffectivePolicy, LayerOrigin};
pub use sandbox::{DryRun, Sandbox, SandboxChild, Spec};
