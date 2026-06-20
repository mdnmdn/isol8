//! Shared effective-policy resolution pipeline for run and introspection commands.

use anyhow::Result;

use crate::cli::RunArgs;
use crate::env;
use crate::filter::RunContext;
use crate::home::{self, EffectiveHome};
use crate::profile::{self, LayerRegistry, Profile};

/// Fully resolved policy for a confined command.
pub struct EffectivePolicy {
    pub layer_names: Vec<String>,
    pub profile: Profile,
    pub env: std::collections::HashMap<String, String>,
    pub home: EffectiveHome,
}

/// Resolve layer stack, home, merged profile, and env without spawning.
pub fn effective_policy(run: &RunArgs) -> Result<EffectivePolicy> {
    let registry = LayerRegistry::load(run.profile_paths())?;
    let ctx = RunContext::from_cmd(&run.cmd);
    let layer_names = profile::select_layer_names(run, &registry, &ctx)?;
    let all = registry.profiles();
    let layers = profile::resolve_requires(&layer_names, &all)?;
    let effective_home = home::resolve(run, &layers)?;
    let merged = profile::load_merged(run, &layers, &effective_home, &ctx)?;
    let env_map = env::build_minimal(&merged, &effective_home.path);
    Ok(EffectivePolicy {
        layer_names,
        profile: merged,
        env: env_map,
        home: effective_home,
    })
}
