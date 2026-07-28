//! Recipes — toolchain integration packages (strategies + home ops + grants + env).
//!
//! A recipe is a **separate document type** from [`crate::profile::Profile`]. It
//! compiles down to path grants, env defaults, and [`crate::plan::HomeOpSpec`]s for
//! a chosen strategy (`share` / `link` / `isolate`). See evo-repo §4 and
//! `_docs/wip/multi-evo-plan.md` Phase 3.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, ResultExt};
use crate::filter::{self, RunContext};
use crate::home::{expand_tilde, REAL_HOME_TOKEN};
use crate::plan::{HomeOpKind, HomeOpSpec};
use crate::profile::{Access, MatchKind, PathGrant, ProfileFilter};

/// Current recipe schema version.
pub const RECIPE_SCHEMA: u32 = 1;

/// Strategy names a recipe may define.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StrategyName {
    /// Symlink to real path, rw on the real path (warm caches).
    Share,
    /// Symlink to real path, ro + writable overlays (version managers).
    Link,
    /// Fresh directory in the replaced home.
    Isolate,
}

impl StrategyName {
    /// Parse from TOML key / cage `strategy = "…"`.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "share" => Ok(StrategyName::Share),
            "link" => Ok(StrategyName::Link),
            "isolate" => Ok(StrategyName::Isolate),
            other => Err(Error::Message(format!(
                "unknown strategy {other:?} (expected share, link, or isolate)"
            ))),
        }
    }

    /// Lowercase label.
    pub fn as_str(self) -> &'static str {
        match self {
            StrategyName::Share => "share",
            StrategyName::Link => "link",
            StrategyName::Isolate => "isolate",
        }
    }
}

/// Optional detection metadata (`@cage detect` on the host).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Detect {
    /// Path probe (typically under real home, e.g. `~/.nvm` or `#HOME/.nvm`).
    pub probe_path: Option<String>,
    /// Optional version command (host; trust-gated).
    pub version_cmd: Option<String>,
}

/// Optional verify metadata (`@cage verify` inside the cage).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Verify {
    /// Smoke-test command.
    pub cmd: Option<String>,
    /// Optional expected stdout regex.
    pub expect: Option<String>,
}

/// One named strategy body inside a recipe.
///
/// A strategy name may carry several bodies with **disjoint** `filter`s, so one
/// recipe can express per-platform internals without splitting into variant
/// files — write `[[strategies.link]]` twice with different selectors. The
/// selector is authoritative here exactly as it is for a recipe or a profile
/// layer; a body with no filter matches every context.
#[derive(Debug, Clone, Serialize)]
pub struct Strategy {
    /// Platform selector for this body; `None` matches every run context.
    pub filter: Option<ProfileFilter>,
    /// One-line description of this choice (shown by the wizard).
    pub summary: String,
    /// Why this strategy exceeds the usual trust ceiling, when it does. Surfaced
    /// as a security note before a cage is written; never suppresses the grant.
    pub danger: Option<String>,
    /// Home materialization ops (tokens still present).
    pub home: Vec<HomeOpSpec>,
    /// Path grants (tokens still present).
    pub paths: Vec<PathGrant>,
    /// Env defaults (values may contain `~` / `#HOME` tokens).
    pub env: HashMap<String, String>,
    /// Directories prepended to `PATH` inside the sandbox (tokens still present,
    /// `*` segment globs allowed). Version managers resolve through shims, which
    /// a single-scalar `env` entry cannot express.
    pub path_prepend: Vec<String>,
}

/// A loaded recipe document.
#[derive(Debug, Clone, Serialize)]
pub struct Recipe {
    /// Schema version.
    pub schema: u32,
    /// Canonical id (e.g. `toolchains/nvm`).
    pub id: String,
    /// One-line summary.
    pub summary: String,
    /// Free-form labels for wizard grouping and registry search.
    pub tags: Vec<String>,
    /// Profile layers this recipe needs in the stack (same semantics as a
    /// layer's own `requires`); pulled in when the recipe is applied.
    pub requires: Vec<String>,
    /// Platform selector (authoritative; filename is convention only).
    pub filter: Option<ProfileFilter>,
    /// Optional detection.
    pub detect: Detect,
    /// Optional verification.
    pub verify: Verify,
    /// Named strategies.
    pub strategies: HashMap<StrategyName, Vec<Strategy>>,
    /// Default strategy when the cage omits one.
    pub default_strategy: Option<StrategyName>,
    /// Source path or `"builtin:<id>"`.
    pub source: String,
}

/// Cage/Spec selection: recipe id + strategy name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolchainChoice {
    /// Recipe id (`toolchains/nvm` or bare `nvm` → `toolchains/nvm`).
    pub id: String,
    /// Strategy to apply.
    pub strategy: StrategyName,
}

impl ToolchainChoice {
    /// Normalize a cage key + strategy string into a choice.
    pub fn new(key: &str, strategy: &str) -> Result<Self> {
        Ok(Self {
            id: normalize_recipe_id(key),
            strategy: StrategyName::parse(strategy)?,
        })
    }
}

/// Normalize a cage toolchain key to a recipe id.
///
/// Bare names become `toolchains/<name>`; ids that already contain `/` pass through.
pub fn normalize_recipe_id(key: &str) -> String {
    let key = key.trim();
    if key.contains('/') {
        key.to_string()
    } else {
        format!("toolchains/{key}")
    }
}

/// Render a recipe id as a cage `[toolchains.<key>]` key.
///
/// Inverse of [`normalize_recipe_id`]: `toolchains/nvm` → `nvm`. Ids that keep a
/// `/` (e.g. `integrations/gh-cli`) are not TOML bare keys and are quoted.
pub fn toolchain_key(id: &str) -> String {
    let short = id.strip_prefix("toolchains/").unwrap_or(id);
    let bare = !short.is_empty()
        && short
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        short.to_string()
    } else {
        // ponytail: Rust debug escaping == TOML basic string for `"` and `\`.
        format!("{short:?}")
    }
}

/// Compiled contribution of one recipe strategy (tokens still present where noted).
#[derive(Debug, Clone, Serialize)]
pub struct RecipeContribution {
    /// Recipe id.
    pub id: String,
    /// Strategy applied.
    pub strategy: StrategyName,
    /// Home ops to append to the materialization plan.
    pub home_ops: Vec<HomeOpSpec>,
    /// Path grants (unexpanded tokens).
    pub paths: Vec<PathGrant>,
    /// Env defaults (unexpanded values).
    pub env: HashMap<String, String>,
    /// `PATH` entries to prepend (unexpanded tokens, `*` globs unresolved).
    pub path_prepend: Vec<String>,
    /// Profile layers the recipe requires in the stack.
    pub requires: Vec<String>,
    /// Danger note declared by the chosen strategy, if any.
    pub danger: Option<String>,
}

// --- TOML wire format ---

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipeFile {
    #[serde(default = "default_schema")]
    schema: u32,
    id: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    filter: Option<ProfileFilter>,
    #[serde(default)]
    default_strategy: Option<String>,
    #[serde(default)]
    detect: Option<DetectFile>,
    #[serde(default)]
    verify: Option<VerifyFile>,
    #[serde(default)]
    strategies: HashMap<String, toml::Value>,
}

fn default_schema() -> u32 {
    RECIPE_SCHEMA
}

fn default_kind() -> String {
    "recipe".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetectFile {
    #[serde(default)]
    probe: Option<ProbeFile>,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeFile {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyFile {
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    expect: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrategyFile {
    #[serde(default)]
    filter: Option<ProfileFilter>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    danger: Option<String>,
    #[serde(default)]
    home: Vec<HomeOpFile>,
    #[serde(default)]
    paths: Vec<PathGrantFile>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    path_prepend: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HomeOpFile {
    kind: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathGrantFile {
    path: String,
    access: Access,
    #[serde(default, rename = "match")]
    r#match: MatchKind,
}

/// Accept both `[strategies.link]` (one body) and `[[strategies.link]]` (platform
/// variants).
///
/// Deliberately hand-dispatched rather than `#[serde(untagged)]`: an untagged
/// enum reports a field typo as "data did not match any variant", burying the
/// actual mistake. Matching on the value shape first means each body is
/// deserialized against exactly one struct, so `deny_unknown_fields` can name
/// the offending key.
fn parse_strategy_bodies(
    recipe_id: &str,
    name: &str,
    value: toml::Value,
) -> Result<Vec<StrategyFile>> {
    match value {
        toml::Value::Array(items) => items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                item.try_into::<StrategyFile>().map_err(|e| {
                    Error::Message(format!(
                        "recipe '{recipe_id}': [[strategies.{name}]] body {}: {e}",
                        i + 1
                    ))
                })
            })
            .collect(),
        table @ toml::Value::Table(_) => Ok(vec![table.try_into::<StrategyFile>().map_err(|e| {
            Error::Message(format!("recipe '{recipe_id}': [strategies.{name}]: {e}"))
        })?]),
        other => Err(Error::Message(format!(
            "recipe '{recipe_id}': strategy {name:?} must be a table or an array of tables (got {})",
            other.type_str()
        ))),
    }
}

/// Parse a recipe from TOML text.
pub fn parse_recipe(body: &str, source: &str) -> Result<Recipe> {
    let file: RecipeFile = toml::from_str(body)
        .map_err(|e| Error::Message(format!("parsing recipe '{source}': {e}")))?;

    if file.schema != RECIPE_SCHEMA {
        return Err(Error::Message(format!(
            "recipe '{source}': unsupported schema {} (expected {RECIPE_SCHEMA})",
            file.schema
        )));
    }
    if file.kind != "recipe" {
        return Err(Error::Message(format!(
            "recipe '{source}': kind must be \"recipe\" (got {:?})",
            file.kind
        )));
    }
    if file.id.is_empty() {
        return Err(Error::Message(format!(
            "recipe '{source}': id must not be empty"
        )));
    }
    if file.strategies.is_empty() {
        return Err(Error::Message(format!(
            "recipe '{}': at least one [strategies.*] is required",
            file.id
        )));
    }

    let mut strategies: HashMap<StrategyName, Vec<Strategy>> = HashMap::new();
    for (name, entry) in file.strategies {
        let sn = StrategyName::parse(&name)
            .map_err(|e| Error::Message(format!("recipe '{}': strategy key: {e}", file.id)))?;
        let bodies = parse_strategy_bodies(&file.id, &name, entry)?;
        if bodies.is_empty() {
            return Err(Error::Message(format!(
                "recipe '{}': strategy {:?} has no body",
                file.id, name
            )));
        }
        let mut variants = Vec::with_capacity(bodies.len());
        for body in bodies {
            let home = body
                .home
                .into_iter()
                .map(|op| parse_home_op(&file.id, op))
                .collect::<Result<Vec<_>>>()?;
            let paths = body
                .paths
                .into_iter()
                .map(|p| PathGrant {
                    path: p.path,
                    access: p.access,
                    r#match: p.r#match,
                })
                .collect();
            variants.push(Strategy {
                filter: body.filter,
                summary: body.summary,
                danger: body.danger,
                home,
                paths,
                env: body.env,
                path_prepend: body.path_prepend,
            });
        }
        // Ambiguity is an error, not a precedence question: if two bodies could
        // both match one platform, the recipe author has to say which they meant.
        for i in 0..variants.len() {
            for j in (i + 1)..variants.len() {
                if filters_overlap(variants[i].filter.as_ref(), variants[j].filter.as_ref()) {
                    return Err(Error::Message(format!(
                        "recipe '{}': strategy {:?} has overlapping filters on bodies {} and {} \
                         (selectors across variants of one strategy must be disjoint)",
                        file.id,
                        name,
                        i + 1,
                        j + 1
                    )));
                }
            }
        }
        strategies.insert(sn, variants);
    }

    let default_strategy =
        match file.default_strategy {
            Some(s) => Some(StrategyName::parse(&s).map_err(|e| {
                Error::Message(format!("recipe '{}': default_strategy: {e}", file.id))
            })?),
            None => None,
        };
    if let Some(ds) = default_strategy {
        if !strategies.contains_key(&ds) {
            return Err(Error::Message(format!(
                "recipe '{}': default_strategy {:?} is not defined in [strategies.*]",
                file.id,
                ds.as_str()
            )));
        }
    }

    let detect = file
        .detect
        .map(|d| Detect {
            probe_path: d.probe.and_then(|p| p.path),
            version_cmd: d.version,
        })
        .unwrap_or_default();
    let verify = file
        .verify
        .map(|v| Verify {
            cmd: v.cmd,
            expect: v.expect,
        })
        .unwrap_or_default();

    Ok(Recipe {
        schema: file.schema,
        id: file.id,
        summary: file.summary,
        tags: file.tags,
        requires: file.requires,
        filter: file.filter,
        detect,
        verify,
        strategies,
        default_strategy,
        source: source.to_string(),
    })
}

fn parse_home_op(recipe_id: &str, op: HomeOpFile) -> Result<HomeOpSpec> {
    let kind = match op.kind.trim().to_ascii_lowercase().as_str() {
        "link" => HomeOpKind::Link,
        "mkdir" => HomeOpKind::Mkdir,
        "seed-ro" | "seed_ro" => HomeOpKind::SeedRo,
        "copy" => HomeOpKind::Copy,
        other => {
            return Err(Error::Message(format!(
                "recipe '{recipe_id}': unknown home op kind {other:?}"
            )));
        }
    };
    match kind {
        HomeOpKind::Mkdir => {
            let path = op.path.ok_or_else(|| {
                Error::Message(format!("recipe '{recipe_id}': mkdir requires `path`"))
            })?;
            Ok(HomeOpSpec::mkdir(path))
        }
        HomeOpKind::Link => {
            let from = op.from.ok_or_else(|| {
                Error::Message(format!("recipe '{recipe_id}': link requires `from`"))
            })?;
            let to = op.to.ok_or_else(|| {
                Error::Message(format!("recipe '{recipe_id}': link requires `to`"))
            })?;
            Ok(HomeOpSpec::link(from, to))
        }
        HomeOpKind::SeedRo => {
            let from = op.from.ok_or_else(|| {
                Error::Message(format!("recipe '{recipe_id}': seed-ro requires `from`"))
            })?;
            let to = op.to.ok_or_else(|| {
                Error::Message(format!("recipe '{recipe_id}': seed-ro requires `to`"))
            })?;
            Ok(HomeOpSpec::seed_ro(from, to))
        }
        HomeOpKind::Copy => {
            let from = op.from.ok_or_else(|| {
                Error::Message(format!("recipe '{recipe_id}': copy requires `from`"))
            })?;
            let to = op.to.ok_or_else(|| {
                Error::Message(format!("recipe '{recipe_id}': copy requires `to`"))
            })?;
            Ok(HomeOpSpec::copy(from, to))
        }
    }
}

/// Load a recipe file from disk.
pub fn load_from_path(path: &Path) -> Result<Recipe> {
    let body =
        std::fs::read_to_string(path).ctx(|| format!("reading recipe '{}'", path.display()))?;
    parse_recipe(&body, &path.display().to_string())
}

// Built-in recipes — generated by build.rs from recipes/**/*.toml.
include!(concat!(env!("OUT_DIR"), "/recipes_embedded.rs"));

/// In-memory recipe catalog: id → platform variants.
#[derive(Debug, Clone, Default)]
pub struct RecipeRegistry {
    /// Candidates per id (disjoint filters).
    by_id: HashMap<String, Vec<Recipe>>,
}

impl RecipeRegistry {
    /// Load builtins + user config dir + offline registry caches + explicit
    /// recipe paths (later wins on identical id+filter; new variants append).
    ///
    /// Registry roots are discovered from `isol8.toml` `[registries]` + the
    /// lockfile / path specs — **no network**. Missing caches are skipped.
    pub fn load(recipe_paths: &[String]) -> Result<Self> {
        Self::load_with_registry_dirs(recipe_paths, &offline_registry_recipe_dirs())
    }

    /// Like [`Self::load`], but with explicit extra registry recipe directories
    /// `(source_label, path)` — used by tests and `@registry` tooling.
    ///
    /// `source_label` is written onto each recipe's `source` field when loading
    /// from that directory (e.g. `registry:official:fixture`). When empty, the
    /// filesystem path is used (local trust).
    pub fn load_with_registry_dirs(
        recipe_paths: &[String],
        registry_dirs: &[(String, PathBuf)],
    ) -> Result<Self> {
        let mut reg = Self::default();

        let mut builtins = Self::default();
        for (id, body) in BUILTIN_RECIPES {
            let r = parse_recipe(body, &format!("builtin:{id}"))?;
            builtins.insert(r)?;
        }
        reg.merge_stage(builtins)?;

        if let Some(dir) = user_recipes_dir() {
            let mut stage = Self::default();
            load_dir_into(&mut stage, &dir, None)?;
            reg.merge_stage(stage)?;
        }
        for (label, dir) in registry_dirs {
            if dir.is_dir() {
                let prefix = if label.is_empty() {
                    None
                } else {
                    Some(label.as_str())
                };
                let mut stage = Self::default();
                load_dir_into(&mut stage, dir, prefix)?;
                reg.merge_stage(stage)?;
            }
        }
        for p in recipe_paths {
            let path = PathBuf::from(p);
            let mut stage = Self::default();
            if path.is_dir() {
                load_dir_into(&mut stage, &path, None)?;
            } else if path.is_file() {
                stage.insert(load_from_path(&path)?)?;
            } else {
                return Err(Error::Message(format!(
                    "recipe path not found: '{}'",
                    path.display()
                )));
            }
            reg.merge_stage(stage)?;
        }
        reg.validate_disjoint()?;
        Ok(reg)
    }

    fn insert(&mut self, recipe: Recipe) -> Result<()> {
        self.by_id
            .entry(recipe.id.clone())
            .or_default()
            .push(recipe);
        Ok(())
    }

    /// Fold one load stage (builtins, user dir, one registry, one `--recipe-path`)
    /// into this registry: a later stage **replaces** any variant it overlaps, so
    /// a registry can ship a better `toolchains/cargo` than the embedded one.
    /// Overlap *within* a stage stays an error — that one is an authoring mistake
    /// with no ordering to resolve it.
    fn merge_stage(&mut self, stage: RecipeRegistry) -> Result<()> {
        stage.validate_disjoint()?;
        for (id, variants) in stage.by_id {
            let slot = self.by_id.entry(id).or_default();
            for v in variants {
                slot.retain(|e| !filters_overlap(e.filter.as_ref(), v.filter.as_ref()));
                slot.push(v);
            }
        }
        Ok(())
    }

    /// Ensure variants of one id have disjoint os/arch selectors.
    pub fn validate_disjoint(&self) -> Result<()> {
        for (id, variants) in &self.by_id {
            for i in 0..variants.len() {
                for j in (i + 1)..variants.len() {
                    if filters_overlap(variants[i].filter.as_ref(), variants[j].filter.as_ref()) {
                        return Err(Error::Message(format!(
                            "recipe '{id}': overlapping platform selectors between {} and {} \
                             (variants of one id must be disjoint)",
                            variants[i].source, variants[j].source
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Resolve a recipe id for the current run context (filter match).
    pub fn resolve(&self, id: &str, ctx: &RunContext) -> Result<&Recipe> {
        let id = normalize_recipe_id(id);
        let Some(variants) = self.by_id.get(&id) else {
            return Err(Error::Message(format!(
                "unknown recipe '{id}' (no builtin or local recipe)"
            )));
        };
        let matches: Vec<&Recipe> = variants
            .iter()
            .filter(|r| match &r.filter {
                None => true,
                Some(f) => filter::filter_matches(f, ctx),
            })
            .collect();
        match matches.as_slice() {
            [] => Err(Error::Message(format!(
                "recipe '{id}' has no variant matching this platform (os={}, arch={})",
                ctx.os, ctx.arch
            ))),
            [one] => Ok(*one),
            many => Err(Error::Message(format!(
                "recipe '{id}': {} variants match this platform (filters must be disjoint)",
                many.len()
            ))),
        }
    }

    /// List all recipe ids (sorted).
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.by_id.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Compile a toolchain choice into a contribution.
    pub fn compile(
        &self,
        choice: &ToolchainChoice,
        ctx: &RunContext,
    ) -> Result<RecipeContribution> {
        let recipe = self.resolve(&choice.id, ctx)?;
        let variants = recipe.strategies.get(&choice.strategy).ok_or_else(|| {
            let mut available: Vec<&str> = recipe.strategies.keys().map(|s| s.as_str()).collect();
            available.sort_unstable();
            Error::Message(format!(
                "recipe '{}': strategy {:?} not defined (available: {})",
                recipe.id,
                choice.strategy.as_str(),
                available.join(", ")
            ))
        })?;
        // Select the body whose selector matches this run. Parse-time validation
        // guarantees the candidates are disjoint, so at most one can match.
        let matching: Vec<&Strategy> = variants
            .iter()
            .filter(|s| match &s.filter {
                None => true,
                Some(f) => filter::filter_matches(f, ctx),
            })
            .collect();
        let strategy = match matching.as_slice() {
            [one] => *one,
            [] => {
                return Err(Error::Message(format!(
                    "recipe '{}': strategy {:?} has no body matching this platform \
                     (os={}, arch={})",
                    recipe.id,
                    choice.strategy.as_str(),
                    ctx.os,
                    ctx.arch
                )));
            }
            many => {
                return Err(Error::Message(format!(
                    "recipe '{}': strategy {:?} has {} bodies matching this platform \
                     (filters must be disjoint)",
                    recipe.id,
                    choice.strategy.as_str(),
                    many.len()
                )));
            }
        };
        Ok(RecipeContribution {
            id: recipe.id.clone(),
            strategy: choice.strategy,
            home_ops: strategy.home.clone(),
            paths: strategy.paths.clone(),
            env: strategy.env.clone(),
            path_prepend: strategy.path_prepend.clone(),
            requires: recipe.requires.clone(),
            danger: strategy.danger.clone(),
        })
    }

    /// Compile all choices (order preserved).
    pub fn compile_all(
        &self,
        choices: &[ToolchainChoice],
        ctx: &RunContext,
    ) -> Result<Vec<RecipeContribution>> {
        choices.iter().map(|c| self.compile(c, ctx)).collect()
    }
}

fn filters_overlap(a: Option<&ProfileFilter>, b: Option<&ProfileFilter>) -> bool {
    // None means "all platforms" — overlaps everything.
    let (a, b) = match (a, b) {
        (None, _) | (_, None) => return true,
        (Some(a), Some(b)) => (a, b),
    };
    let os_overlap =
        a.os.is_empty() || b.os.is_empty() || a.os.iter().any(|o| b.os.iter().any(|p| p == o));
    let arch_overlap = a.arch.is_empty()
        || b.arch.is_empty()
        || a.arch.iter().any(|o| b.arch.iter().any(|p| p == o));
    os_overlap && arch_overlap
}

fn user_recipes_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("APPDATA")
                    .filter(|h| !h.is_empty())
                    .map(PathBuf::from)
            } else {
                None
            }
        })
        .map(|h| h.join("isol8").join("recipes"))
}

fn load_dir_into(reg: &mut RecipeRegistry, dir: &Path, source_prefix: Option<&str>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    load_dir_rec(reg, dir, source_prefix)
}

fn load_dir_rec(reg: &mut RecipeRegistry, dir: &Path, source_prefix: Option<&str>) -> Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            load_dir_rec(reg, &p, source_prefix)?;
        } else if p.extension().and_then(|e| e.to_str()) == Some("toml") {
            match load_from_path(&p) {
                Ok(mut r) => {
                    if let Some(prefix) = source_prefix {
                        // prefix is typically `registry:<trust>:<name>`
                        r.source = format!("{prefix}:{}", r.id);
                    }
                    reg.insert(r)?;
                }
                Err(e) => {
                    // Registry trees may contain profiles / future schema —
                    // skip unreadable TOML rather than failing the whole load.
                    // A file that calls itself a recipe and still fails is a real
                    // problem the user must see: silence here is how 20 rejected
                    // recipes looked exactly like an empty registry.
                    if source_prefix.is_some() {
                        if declares_recipe_kind(&p) {
                            eprintln!("warning: skipping recipe '{}': {e}", p.display());
                        }
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

/// Callback that supplies offline registry recipe directories `(source_label, dir)`.
///
/// Installed by the `isol8` facade or `isol8-cli` when the registry crate is
/// linked — keeps `isol8-core` free of a dependency on `isol8-registry`.
pub type OfflineRegistryProvider = fn() -> Vec<(String, PathBuf)>;

static OFFLINE_REGISTRY_PROVIDER: OnceLock<OfflineRegistryProvider> = OnceLock::new();

/// Register the offline-registry directory provider (idempotent; first wins).
pub fn set_offline_registry_provider(f: OfflineRegistryProvider) {
    let _ = OFFLINE_REGISTRY_PROVIDER.set(f);
}

/// Discover offline registry recipe directories via the registered provider.
///
/// Returns empty when no provider is registered (core-only embeds).
fn offline_registry_recipe_dirs() -> Vec<(String, PathBuf)> {
    OFFLINE_REGISTRY_PROVIDER
        .get()
        .map(|f| f())
        .unwrap_or_default()
}

/// True when a TOML file explicitly declares `kind = "recipe"`. Used to decide
/// whether a parse failure in a registry tree is worth a warning (profiles and
/// bundles live in the same tree and are expected to be skipped).
fn declares_recipe_kind(path: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(path) else {
        return false;
    };
    body.parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("kind").and_then(|k| k.as_str().map(str::to_string)))
        .is_some_and(|k| k == "recipe")
}

/// Expand env value tokens: `#HOME` → real home, `~` → effective home.
pub fn expand_env_value(raw: &str, real_home: &Path, effective_home: &Path) -> String {
    let s = if raw.contains(REAL_HOME_TOKEN) {
        raw.replace(REAL_HOME_TOKEN, &real_home.to_string_lossy())
    } else {
        raw.to_string()
    };
    expand_tilde(&s, effective_home)
}

/// Expand one `path_prepend` entry into concrete directories.
///
/// Tokens expand as everywhere else (`#HOME` → real home, `~` → effective home),
/// then a `*` path segment is globbed against the filesystem: version managers
/// keep their shims under a per-version directory (`~/.nvm/versions/node/*/bin`)
/// that only exists at resolve time. A glob matching nothing yields no entries;
/// a literal path is kept even if absent, because home materialization may still
/// create it after the policy is computed.
///
/// `planned_links` are the `(link_path, target)` pairs the home plan will create.
/// A glob under a not-yet-created link is resolved against its target and mapped
/// back, so a cold home yields the same `PATH` as a warm one — otherwise the
/// first run of a fresh cage silently has no node on `PATH` and the second works.
///
/// ponytail: `*` matches one whole segment — no `node-*`, no `**`, no character
/// classes. Pull in a glob crate if a recipe ever needs more.
pub fn expand_path_prepend(
    raw: &str,
    real_home: &Path,
    effective_home: &Path,
    planned_links: &[(PathBuf, PathBuf)],
) -> Vec<String> {
    let expanded = expand_env_value(raw, real_home, effective_home);
    if !expanded.contains('*') {
        return vec![expanded];
    }
    // Redirect through a planned symlink, remembering how to map results back.
    let mut probe = expanded.clone();
    let mut mapping: Option<(String, String)> = None;
    for (link, target) in planned_links {
        let l = link.to_string_lossy();
        if expanded.starts_with(&format!("{l}/")) {
            let t = target.to_string_lossy().into_owned();
            probe = expanded.replacen(l.as_ref(), &t, 1);
            mapping = Some((t, l.into_owned()));
            break;
        }
    }
    let path = PathBuf::from(&probe);
    let mut comps = path.components();
    // Anchor on the path root (or cwd for a relative entry), then walk segments.
    let root = match comps.next() {
        Some(std::path::Component::RootDir) => PathBuf::from("/"),
        Some(std::path::Component::Prefix(p)) => PathBuf::from(p.as_os_str()),
        Some(other) => PathBuf::from(other.as_os_str()),
        None => return Vec::new(),
    };
    let rest: Vec<String> = comps
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    glob_segments(root, &rest)
        .into_iter()
        .map(|p| {
            let s = p.to_string_lossy().into_owned();
            match &mapping {
                Some((target, link)) => s.replacen(target.as_str(), link.as_str(), 1),
                None => s,
            }
        })
        .collect()
}

/// Walk `rest` under `base`, expanding a whole-segment `*` against real directories.
fn glob_segments(base: PathBuf, rest: &[String]) -> Vec<PathBuf> {
    let Some((seg, tail)) = rest.split_first() else {
        return vec![base];
    };
    if seg != "*" {
        return glob_segments(base.join(seg), tail);
    }
    let Ok(rd) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut kids: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    // Lexical order so the resulting PATH is deterministic across runs.
    kids.sort();
    kids.into_iter()
        .flat_map(|k| glob_segments(k, tail))
        .collect()
}

/// Resolve strategy for a choice, applying recipe `default_strategy` when the
/// choice strategy was left unspecified at the cage layer — callers always pass
/// an explicit strategy today; this helper is for future wizard use.
pub fn resolve_strategy(recipe: &Recipe, requested: Option<StrategyName>) -> Result<StrategyName> {
    if let Some(s) = requested {
        if recipe.strategies.contains_key(&s) {
            return Ok(s);
        }
        return Err(Error::Message(format!(
            "recipe '{}': strategy {:?} not defined",
            recipe.id,
            s.as_str()
        )));
    }
    if let Some(ds) = recipe.default_strategy {
        return Ok(ds);
    }
    // Prefer link > share > isolate as a stable fallback order.
    for pref in [
        StrategyName::Link,
        StrategyName::Share,
        StrategyName::Isolate,
    ] {
        if recipe.strategies.contains_key(&pref) {
            return Ok(pref);
        }
    }
    Err(Error::Message(format!(
        "recipe '{}': no strategies defined",
        recipe.id
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    // r## so `"#HOME/..."` does not terminate the raw string.
    const NVM_RECIPE: &str = r##"
schema = 1
id = "toolchains/nvm"
kind = "recipe"
filter = { os = ["macos", "linux"] }
summary = "Node Version Manager"
default_strategy = "link"

[detect]
probe = { path = "~/.nvm" }

[verify]
cmd = "node --version"
expect = "^v\\d+"

[strategies.link]
home = [{ kind = "link", from = "#HOME/.nvm", to = "~/.nvm" }]
paths = [
  { path = "#HOME/.nvm/versions", access = "ro" },
  { path = "#HOME/.nvm/alias", access = "rw" },
]
env = { NVM_DIR = "~/.nvm" }

[strategies.isolate]
home = [{ kind = "mkdir", path = "~/.nvm" }]
paths = [{ path = "~/.nvm", access = "rw" }]
env = { NVM_DIR = "~/.nvm" }
"##;

    #[test]
    fn parse_and_compile_link() {
        let r = parse_recipe(NVM_RECIPE, "test").unwrap();
        assert_eq!(r.id, "toolchains/nvm");
        assert!(r.strategies.contains_key(&StrategyName::Link));
        assert_eq!(r.default_strategy, Some(StrategyName::Link));

        let mut reg = RecipeRegistry::default();
        reg.insert(r).unwrap();
        let ctx = RunContext {
            cmd: vec!["node".into()],
            os: "macos".into(),
            arch: "aarch64".into(),
        };
        let c = reg
            .compile(
                &ToolchainChoice {
                    id: "toolchains/nvm".into(),
                    strategy: StrategyName::Link,
                },
                &ctx,
            )
            .unwrap();
        assert_eq!(c.home_ops.len(), 1);
        assert_eq!(c.home_ops[0].kind, HomeOpKind::Link);
        assert_eq!(c.paths.len(), 2);
        assert_eq!(c.env.get("NVM_DIR").map(String::as_str), Some("~/.nvm"));
    }

    #[test]
    fn normalize_bare_id() {
        assert_eq!(normalize_recipe_id("nvm"), "toolchains/nvm");
        assert_eq!(normalize_recipe_id("toolchains/nvm"), "toolchains/nvm");
    }

    #[test]
    fn expand_env_tokens() {
        assert_eq!(
            expand_env_value("~/.nvm", Path::new("/real"), Path::new("/eff")),
            "/eff/.nvm"
        );
        assert_eq!(
            expand_env_value("#HOME/.nvm", Path::new("/real"), Path::new("/eff")),
            "/real/.nvm"
        );
    }

    #[test]
    fn platform_mismatch_errors() {
        let r = parse_recipe(NVM_RECIPE, "test").unwrap();
        let mut reg = RecipeRegistry::default();
        reg.insert(r).unwrap();
        let ctx = RunContext {
            cmd: vec![],
            os: "windows".into(),
            arch: "x86_64".into(),
        };
        let err = reg.resolve("toolchains/nvm", &ctx).unwrap_err().to_string();
        assert!(err.contains("no variant"), "{err}");
    }

    #[test]
    fn overlapping_filters_rejected() {
        let a = parse_recipe(NVM_RECIPE, "a").unwrap();
        let b = parse_recipe(
            r##"
schema = 1
id = "toolchains/nvm"
kind = "recipe"
filter = { os = ["macos"] }
[strategies.link]
paths = []
"##,
            "b",
        )
        .unwrap();
        let mut reg = RecipeRegistry::default();
        reg.insert(a).unwrap();
        reg.insert(b).unwrap();
        let err = reg.validate_disjoint().unwrap_err().to_string();
        assert!(err.contains("overlapping"), "{err}");
    }

    // Registry-schema fields: metadata the parser must accept, plus path_prepend.
    const EXTENDED_RECIPE: &str = r##"
schema = 1
id = "toolchains/pyenv"
kind = "recipe"
summary = "pyenv"
tags = ["runtime", "version-manager"]
requires = ["integrations/git"]
default_strategy = "link"

[strategies.link]
summary = "Execute the host's interpreters; new installs land in the replaced home"
danger = "grants rw on the real pyenv root"
home = [{ kind = "link", from = "#HOME/.pyenv", to = "~/.pyenv" }]
paths = [{ path = "#HOME/.pyenv", access = "ro" }]
path_prepend = ["~/.pyenv/shims", "~/.pyenv/bin"]
"##;

    #[test]
    fn parses_registry_metadata_and_path_prepend() {
        let r = parse_recipe(EXTENDED_RECIPE, "test").unwrap();
        assert_eq!(r.tags, vec!["runtime", "version-manager"]);
        assert_eq!(r.requires, vec!["integrations/git"]);

        let mut reg = RecipeRegistry::default();
        reg.insert(r).unwrap();
        let ctx = RunContext {
            cmd: vec![],
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
        };
        let c = reg
            .compile(
                &ToolchainChoice {
                    id: "toolchains/pyenv".into(),
                    strategy: StrategyName::Link,
                },
                &ctx,
            )
            .unwrap();
        assert_eq!(c.path_prepend, vec!["~/.pyenv/shims", "~/.pyenv/bin"]);
        assert_eq!(c.requires, vec!["integrations/git"]);
        assert!(c.danger.as_deref().unwrap().contains("rw on the real"));
    }

    #[test]
    fn path_prepend_expands_tokens_and_globs() {
        let base = std::env::temp_dir().join(format!("isol8-pp-{}", std::process::id()));
        let versions = base.join(".nvm/versions/node");
        std::fs::create_dir_all(versions.join("v18.20.0/bin")).unwrap();
        std::fs::create_dir_all(versions.join("v22.1.0/bin")).unwrap();
        let eff = base.join("eff-home");

        // Literal entry: kept even though it does not exist yet (materialization
        // may still create it), and `~` resolves against the effective home.
        let lit = expand_path_prepend("~/.local/bin", &base, &eff, &[]);
        assert_eq!(lit, vec![eff.join(".local/bin").to_string_lossy()]);

        // Glob entry: one segment, sorted, only real directories.
        let globbed = expand_path_prepend("#HOME/.nvm/versions/node/*/bin", &base, &eff, &[]);
        assert_eq!(
            globbed,
            vec![
                versions.join("v18.20.0/bin").to_string_lossy().into_owned(),
                versions.join("v22.1.0/bin").to_string_lossy().into_owned(),
            ]
        );

        // Glob matching nothing contributes nothing.
        assert!(expand_path_prepend("#HOME/.nope/*/bin", &base, &eff, &[]).is_empty());

        // Cold home: `~/.nvm` is only a *planned* link, so the glob resolves via
        // the target and the result is mapped back to the replaced-home form.
        let planned = vec![(eff.join(".nvm"), base.join(".nvm"))];
        let cold = expand_path_prepend("~/.nvm/versions/node/*/bin", &base, &eff, &planned);
        assert_eq!(
            cold,
            vec![
                eff.join(".nvm/versions/node/v18.20.0/bin")
                    .to_string_lossy()
                    .into_owned(),
                eff.join(".nvm/versions/node/v22.1.0/bin")
                    .to_string_lossy()
                    .into_owned(),
            ]
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn only_self_declared_recipes_are_worth_a_warning() {
        let dir = std::env::temp_dir().join(format!("isol8-kind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let recipe = dir.join("r.toml");
        std::fs::write(&recipe, "kind = \"recipe\"\nid = \"x\"\n").unwrap();
        let profile = dir.join("p.toml");
        std::fs::write(&profile, "paths = []\n").unwrap();
        let bundle = dir.join("b.toml");
        std::fs::write(&bundle, "kind = \"bundle\"\n").unwrap();

        assert!(declares_recipe_kind(&recipe));
        assert!(!declares_recipe_kind(&profile));
        assert!(!declares_recipe_kind(&bundle));
        assert!(!declares_recipe_kind(&dir.join("missing.toml")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn later_source_replaces_an_overlapping_variant() {
        let mut reg = RecipeRegistry::default();
        let mut builtin = RecipeRegistry::default();
        builtin
            .insert(parse_recipe(NVM_RECIPE, "builtin:toolchains/nvm").unwrap())
            .unwrap();
        reg.merge_stage(builtin).unwrap();

        let mut from_registry = RecipeRegistry::default();
        from_registry
            .insert(parse_recipe(NVM_RECIPE, "registry:official:policy:toolchains/nvm").unwrap())
            .unwrap();
        reg.merge_stage(from_registry).unwrap();

        // One variant survives — the later source — instead of an overlap error.
        reg.validate_disjoint().unwrap();
        let variants = &reg.by_id["toolchains/nvm"];
        assert_eq!(variants.len(), 1);
        assert!(variants[0].source.starts_with("registry:"));
    }

    #[test]
    fn builtin_recipes_parse() {
        // At least the embedded set must load (may be empty only if recipes/ missing).
        let reg = RecipeRegistry::load(&[]).unwrap();
        // nvm should be embedded once we add fixtures.
        if reg.by_id.contains_key("toolchains/nvm") {
            let ctx = RunContext {
                cmd: vec![],
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
            };
            // On windows nvm may not match; only check if platform matches.
            let _ = reg.resolve("toolchains/nvm", &ctx);
        }
    }
}
