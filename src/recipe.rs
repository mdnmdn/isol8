//! Recipes — toolchain integration packages (strategies + home ops + grants + env).
//!
//! A recipe is a **separate document type** from [`crate::profile::Profile`]. It
//! compiles down to path grants, env defaults, and [`crate::plan::HomeOpSpec`]s for
//! a chosen strategy (`share` / `link` / `isolate`). See evo-repo §4 and
//! `_docs/wip/multi-evo-plan.md` Phase 3.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result, ResultExt};
use crate::filter::{self, RunContext};
use crate::home::{expand_tilde, REAL_HOME_TOKEN};
use crate::plan::{HomeOpKind, HomeOpSpec};
use crate::profile::{Access, MatchKind, PathGrant, ProfileFilter};

/// Current recipe schema version.
pub const RECIPE_SCHEMA: u32 = 1;

/// Strategy names a recipe may define.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Optional detection metadata (Phase 4 will run probes; parsed today).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detect {
    /// Path probe (typically under real home, e.g. `~/.nvm` or `#HOME/.nvm`).
    pub probe_path: Option<String>,
    /// Optional version command (not executed until Phase 4).
    pub version_cmd: Option<String>,
}

/// Optional verify metadata (Phase 4 will run; parsed today).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
#[derive(Debug, Clone)]
pub struct Strategy {
    /// Platform selector for this body; `None` matches every run context.
    pub filter: Option<ProfileFilter>,
    /// Home materialization ops (tokens still present).
    pub home: Vec<HomeOpSpec>,
    /// Path grants (tokens still present).
    pub paths: Vec<PathGrant>,
    /// Env defaults (values may contain `~` / `#HOME` tokens).
    pub env: HashMap<String, String>,
}

/// A loaded recipe document.
#[derive(Debug, Clone)]
pub struct Recipe {
    /// Schema version.
    pub schema: u32,
    /// Canonical id (e.g. `toolchains/nvm`).
    pub id: String,
    /// One-line summary.
    pub summary: String,
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Compiled contribution of one recipe strategy (tokens still present where noted).
#[derive(Debug, Clone)]
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
    home: Vec<HomeOpFile>,
    #[serde(default)]
    paths: Vec<PathGrantFile>,
    #[serde(default)]
    env: HashMap<String, String>,
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
                home,
                paths,
                env: body.env,
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
    /// Load builtins + user config dir + explicit recipe paths (later wins on
    /// identical id+filter; new variants are appended).
    pub fn load(recipe_paths: &[String]) -> Result<Self> {
        let mut reg = Self::default();
        for (id, body) in BUILTIN_RECIPES {
            let r = parse_recipe(body, &format!("builtin:{id}"))?;
            reg.insert(r)?;
        }
        if let Some(dir) = user_recipes_dir() {
            load_dir_into(&mut reg, &dir)?;
        }
        for p in recipe_paths {
            let path = PathBuf::from(p);
            if path.is_dir() {
                load_dir_into(&mut reg, &path)?;
            } else if path.is_file() {
                reg.insert(load_from_path(&path)?)?;
            } else {
                return Err(Error::Message(format!(
                    "recipe path not found: '{}'",
                    path.display()
                )));
            }
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

fn load_dir_into(reg: &mut RecipeRegistry, dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    load_dir_rec(reg, dir)
}

fn load_dir_rec(reg: &mut RecipeRegistry, dir: &Path) -> Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            load_dir_rec(reg, &p)?;
        } else if p.extension().and_then(|e| e.to_str()) == Some("toml") {
            reg.insert(load_from_path(&p)?)?;
        }
    }
    Ok(())
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
