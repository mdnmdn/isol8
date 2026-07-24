//! `isol8-cli` — CLI surface, cage wizard, meta-commands.
//!
//! Depends on [`isol8_core`] and [`isol8_registry`]. Policy logic stays in core;
//! this crate handles prompts, rendering, and TOML surgery for cages.

#![warn(missing_docs)]

/// Clap parsing, config, diag, and the binary entry point.
pub mod cli;
/// Cage wizard: managed sections, drift protection, non-interactive authoring.
pub mod wizard;

pub use wizard::{
    apply, check_drift, default_strategy_for, expand_bundle, managed_hash, normalize_home,
    parse_tools_list, render, DriftStatus, WizardRequest, WizardResult, WizardState,
};
