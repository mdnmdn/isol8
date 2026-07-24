//! `isol8-core` — confinement engine (profiles, resolve, home plan, backends).
//!
//! This crate has **no** network registry I/O and **no** CLI. Offline registry
//! recipe dirs can be plugged in via [`recipe::set_offline_registry_provider`].

#![warn(missing_docs)]

/// Denial analysis → recipe suggestions (`--analyze`).
pub mod analyze;
/// macOS unified-log denial scrape for `--analyze`.
#[cfg(target_os = "macos")]
pub mod analyze_macos;
/// Per-OS sandbox backends (Seatbelt / Landlock / AppContainer).
pub mod backends;
/// Named local isolation units (cages).
pub mod cage;
/// Injectable ambient context for path tokens and managed homes.
pub mod context;
/// Toolchain detection and cage verification.
pub mod detect;
/// Sanitized environment construction (R3).
pub mod env;
pub mod error;
pub mod filter;
pub mod home;
/// Home materialization plan/apply.
pub mod plan;
/// Profile model, TOML loading, deny-first merge.
pub mod profile;
/// Toolchain recipes: strategies → grants + home ops + env.
pub mod recipe;
pub mod resolve;
pub mod sandbox;

pub use analyze::{AnalysisReport, Denial, DenialAccess};
pub use cage::{Cage, CageOverlay, HomeMode};
pub use context::{Context, Platform};
pub use error::{Error, Result};
pub use plan::{HomeOpKind, HomeOpSpec, HomePlan, PlanAction, PlannedOp};
pub use profile::{Access, MatchKind, PathGrant, Profile};
pub use recipe::{Recipe, RecipeRegistry, StrategyName, ToolchainChoice};
pub use resolve::{confine_executable, effective_policy, EffectivePolicy, LayerOrigin};
pub use sandbox::{DryRun, Sandbox, SandboxChild, Spec};
