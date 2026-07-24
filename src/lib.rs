//! `isol8` — facade crate re-exporting the engine workspace (Phase 9).
//!
//! | Crate | Role |
//! |-------|------|
//! | [`isol8_core`] | Profiles, resolve, home plan, backends, detect/analyze |
//! | [`isol8_registry`] | Offline registries, lockfile, trust (feature `registry`) |
//! | [`isol8_cli`] | CLI + wizard (feature `cli`) |
//!
//! ```no_run
//! let code: i32 = isol8::Sandbox::new()
//!     .profile("base")
//!     .grant_rw("/my/project")
//!     .run(["node", "script.js"])?;
//! # Ok::<(), isol8::Error>(())
//! ```
//!
//! Engine-only: `isol8 = { ..., default-features = false }`.
//! With `registry` (default), call [`ensure_registry_provider`] once if you use
//! config-backed offline registries without going through the CLI binary.

#![warn(missing_docs)]

// Module re-exports (preserve `isol8::profile::…` paths).
#[doc(inline)]
pub use isol8_core::{
    analyze, backends, cage, context, detect, env, error, filter, home, plan, profile, recipe,
    resolve, sandbox,
};

#[cfg(target_os = "macos")]
#[doc(inline)]
pub use isol8_core::analyze_macos;

// Common type re-exports at the crate root.
#[doc(inline)]
pub use isol8_core::{
    Access, AnalysisReport, Cage, CageOverlay, Context, Denial, DenialAccess, DryRun,
    EffectivePolicy, Error, HomeMode, HomeOpKind, HomeOpSpec, HomePlan, LayerOrigin, MatchKind,
    PathGrant, PlanAction, PlannedOp, Platform, Profile, Recipe, RecipeRegistry, Result, Sandbox,
    SandboxChild, Spec, StrategyName, ToolchainChoice,
};

#[doc(inline)]
pub use isol8_core::{confine_executable, effective_policy};

/// Offline recipe/profile registries (feature `registry`).
#[cfg(feature = "registry")]
#[doc(inline)]
pub use isol8_registry as registry;

#[cfg(feature = "registry")]
#[doc(inline)]
pub use isol8_registry::{
    default_cache_root, discover_lockfile_path, discover_offline_recipe_dirs, open_offline,
    parse_registries_from_toml, update_registry, DirSource, Lockfile, ProfileSource, RegistryIndex,
    RegistrySpec, TrustLevel,
};

/// CLI surface (feature `cli`).
#[cfg(feature = "cli")]
pub mod cli {
    #![allow(missing_docs)]
    pub use isol8_cli::cli::*;
}

/// Cage wizard (feature `cli`).
#[cfg(feature = "cli")]
pub mod wizard {
    #![allow(missing_docs)]
    pub use isol8_cli::wizard::*;
}

/// Install offline-registry recipe discovery into the core recipe loader.
///
/// The CLI binary calls this at startup. Library embedders that read
/// `[registries]` from config should call it once before resolving recipes.
#[cfg(feature = "registry")]
pub fn ensure_registry_provider() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        isol8_core::recipe::set_offline_registry_provider(
            isol8_registry::discover_offline_recipe_dirs,
        );
    });
}
