use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;

use crate::cli::RunArgs;

/// Per-path access level. Default is deny (`None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    None,
    Ro,
    Rw,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathGrant {
    pub path: String,
    pub access: Access,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HomeReplace {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub auto_scratch: bool,
    /// Real-home entries to seed read-only into the replacement (e.g. "~/.gitconfig").
    #[serde(default)]
    pub seed: Vec<String>,
}

/// One profile layer as authored in TOML/YAML. Layers declare their dependencies
/// via `requires` (profile inheritance); the set is expanded transitively before
/// merging (see `resolve_requires`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Profile {
    /// Names of layers this one depends on; pulled in transitively, deps first.
    /// Accepts `extends` as an alias.
    #[serde(default, alias = "extends")]
    pub requires: Vec<String>,
    #[serde(default)]
    pub paths: Vec<PathGrant>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub home_replace: Option<HomeReplace>,
}

/// Expand the selected layers over their transitive `requires` graph, returning
/// them in merge order (dependencies before dependents).
///
/// ponytail: stub. Real impl is a DFS topo-sort with cycle detection (error with
/// the cycle path), dedup (each layer once), band-number tiebreak. This is the
/// inheritance resolver that runs *before* `merge`; see _docs/profile-model.md §3.
pub fn resolve_requires(_selected: &[String]) -> Result<Vec<Profile>> {
    todo!("DFS over requires: cycle detection, dedup, topological (deps-first) order")
}

/// Load profile layers named on the CLI, merge them, and fold in the
/// `--add-dirs-*` / `--home` invocation overrides.
///
/// ponytail: stub. Profile discovery (embedded defaults + user TOML dir) and the
/// real merge are not implemented yet — see merge() and spec section 6/7.
pub fn load(_run: &RunArgs) -> Result<Profile> {
    todo!("load + merge profile layers from embedded defaults and user TOML dir")
}

/// Merge layers deny-first into one effective profile.
///
/// ponytail: stub. Intended semantics (spec R2.4/R6): union grants with last
/// explicit grant per path winning, env merged without override, home_replace
/// from the highest layer that sets it.
pub fn merge(_layers: &[Profile]) -> Profile {
    todo!("deny-first union of path grants + env + home_replace")
}
