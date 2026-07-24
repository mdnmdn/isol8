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
//! - [`cage`] — named local isolation units (selection → [`Spec`] fields).
//! - [`context`] — injectable ambient state for token expansion (`~`, `#HOME`, `@managed`).
//! - [`plan`] — home materialization plan/apply (`link` / `mkdir` / `seed-ro` / `copy`).
//! - [`recipe`] — toolchain recipes (strategies → grants + home ops + env).
//! - [`registry`] — offline-by-default recipe/profile sources, lockfile, trust.
//! - [`wizard`] — cage authoring (`@cage new` / `edit`), managed sections, drift.
//! - [`filter`] — conditional layer/policy matching (OS / arch / executable).
//! - [`backends`] — the per-OS [`backends::Backend`] implementations.
//! - [`error`] — the typed [`Error`] and [`Result`] returned by the engine.
//!
//! [`_docs/project-description.md`]: https://github.com/eugene1g/agent-safehouse

#![warn(missing_docs)]

/// Denial analysis → recipe suggestions (`--analyze`, Phase 5).
pub mod analyze;
/// macOS unified-log denial scrape for `--analyze` (Phase 6).
#[cfg(target_os = "macos")]
pub mod analyze_macos;
/// Per-OS sandbox [`backends::Backend`] implementations (Seatbelt / Landlock /
/// AppContainer) plus backend [`backends::select`]ion.
pub mod backends;
/// Named local isolation units (cages): load, discover, compile to Spec fields.
pub mod cage;
/// Injectable ambient context for path tokens and managed homes.
pub mod context;
/// Toolchain detection and cage verification (Phase 4).
pub mod detect;
/// Sanitized environment construction (R3): minimal allowlist, HOME first.
pub mod env;
pub mod error;
pub mod filter;
pub mod home;
/// Home materialization plan/apply (link, mkdir, seed-ro, copy).
pub mod plan;
/// The [`Profile`] model: path grants, capabilities, TOML loading, deny-first merge.
pub mod profile;
/// Toolchain recipes: strategies compile to path grants, env, and home ops.
pub mod recipe;
/// Offline-by-default recipe/profile registries (path/git cache + lockfile).
pub mod registry;
pub mod resolve;
pub mod sandbox;
/// Cage wizard: managed sections, drift protection, non-interactive authoring.
pub mod wizard;

/// CLI surface (arg parsing, config, diag, the binary entry point). Behind the
/// default-on `cli` feature; not part of the stable embedding API.
#[cfg(feature = "cli")]
pub mod cli;

pub use analyze::{AnalysisReport, Denial, DenialAccess};
pub use cage::{Cage, CageOverlay, HomeMode};
pub use context::{Context, Platform};
pub use error::{Error, Result};
pub use plan::{HomeOpKind, HomeOpSpec, HomePlan, PlanAction, PlannedOp};
pub use profile::{Access, MatchKind, PathGrant, Profile};
pub use recipe::{Recipe, RecipeRegistry, StrategyName, ToolchainChoice};
pub use registry::{DirSource, Lockfile, ProfileSource, RegistryIndex, RegistrySpec, TrustLevel};
pub use resolve::{confine_executable, effective_policy, EffectivePolicy, LayerOrigin};
pub use sandbox::{DryRun, Sandbox, SandboxChild, Spec};
