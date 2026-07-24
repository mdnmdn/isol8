//! Cage wizard — non-interactive (and interactive CLI) cage authoring (Phase 8).
//!
//! Builds cage TOML with `# isol8:managed` toolchain sections, optional bundle
//! expansion, and drift protection via `~/.config/isol8/state.toml`.
//!
//! Design: [`_docs/inbox/evo-repo.md`](../_docs/inbox/evo-repo.md) §3.3 / §6.  
//! Plan: [`_docs/wip/multi-evo-plan.md`](../_docs/wip/multi-evo-plan.md) Phase 8.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml_edit::{value, DocumentMut, Item, Table};

use crate::error::{Error, Result, ResultExt};
use crate::profile::Access;
use crate::recipe::{
    normalize_recipe_id, resolve_strategy, Recipe, RecipeRegistry, StrategyName, ToolchainChoice,
};
use crate::registry::{sha256_hex, ProfileSource};

/// Marker comment prefix for wizard-owned toolchain sections (evo-repo §3.3).
pub const MANAGED_MARKER: &str = "isol8:managed";

// ---------------------------------------------------------------------------
// Open decisions (locked — Phase 8)
// ---------------------------------------------------------------------------
//
// 1. Strategy defaults: recipe `default_strategy` first, then id heuristics
//    (caches → share, version managers → link, else isolate / first available).
// 2. `inherit` + toolchains: still record strategy choices; grants on `#HOME`
//    remain meaningful; link/mkdir under `~` hits the real home when not replaced.
// 3. Project-local cages: supported via `--path`; absolute `home` paths get a
//    portability warning only (not an error).

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Inputs for creating or editing a cage.
#[derive(Debug, Clone)]
pub struct WizardRequest {
    /// Cage name (bare identifier).
    pub name: String,
    /// Home mode: `inherit` | `ephemeral` | `managed` | `@managed/<id>` | path.
    pub home: String,
    /// Toolchain choices (strategy already resolved).
    pub tools: Vec<ToolchainChoice>,
    /// Extra `[[dirs]]` grants (path, access).
    pub dirs: Vec<(String, Access)>,
    /// Profile layers; empty → leave profiles = [] (config defaults apply at run).
    pub profiles: Vec<String>,
    /// Directory to write into (default: user cages dir).
    pub out_dir: Option<PathBuf>,
    /// Overwrite existing cage file / ignore managed-section drift.
    pub force: bool,
    /// Path of an existing cage when editing (preserves user sections).
    pub existing_path: Option<PathBuf>,
}

/// Result of rendering / writing a cage.
#[derive(Debug, Clone)]
pub struct WizardResult {
    /// Destination path (written or intended).
    pub path: PathBuf,
    /// Full cage TOML body.
    pub body: String,
    /// Hash of managed toolchain content.
    pub managed_hash: String,
    /// Non-fatal notes (portability, inherit+tools, …).
    pub warnings: Vec<String>,
}

/// Drift check outcome for managed sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftStatus {
    /// No prior state — safe to write.
    Clean,
    /// Hash matches last wizard write.
    Unchanged,
    /// File differs from last wizard write.
    Drift {
        /// Path recorded in state.
        path: PathBuf,
        /// Hash the wizard last wrote.
        expected: String,
        /// Hash of current file's managed sections.
        actual: String,
    },
    /// State entry missing but file exists.
    UnknownExisting,
}

// ---------------------------------------------------------------------------
// Home / tools parsing
// ---------------------------------------------------------------------------

/// Normalize wizard `--home` into a cage `home = "…"` value.
///
/// - `managed` → `@managed/<cage_name>`
/// - `inherit` / `ephemeral` / `@managed/…` / paths pass through (validated)
pub fn normalize_home(cage_name: &str, home: &str) -> Result<String> {
    let h = home.trim();
    match h {
        "" | "inherit" => Ok("inherit".into()),
        "ephemeral" => Ok("ephemeral".into()),
        "managed" => Ok(format!("@managed/{cage_name}")),
        other if other.starts_with("@managed/") => {
            crate::cage::HomeMode::parse(other)?;
            Ok(other.to_string())
        }
        other => {
            crate::cage::HomeMode::parse(other)?;
            Ok(other.to_string())
        }
    }
}

/// Parse `--tools nvm,cargo:share,maven` into choices.
///
/// Bare ids use [`default_strategy_for`] after looking up the recipe when a
/// registry is provided; without a registry, bare ids default to `link`.
pub fn parse_tools_list(raw: &str, reg: Option<&RecipeRegistry>) -> Result<Vec<ToolchainChoice>> {
    let mut out = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (id_raw, strat_opt) = match part.split_once(':') {
            Some((id, s)) => (id.trim(), Some(s.trim())),
            None => (part, None),
        };
        let id = normalize_recipe_id(id_raw);
        let strategy = if let Some(s) = strat_opt {
            StrategyName::parse(s)?
        } else if let Some(reg) = reg {
            match reg.resolve(&id, &crate::filter::RunContext::from_cmd(&[])) {
                Ok(recipe) => default_strategy_for(recipe),
                Err(_) => StrategyName::Link,
            }
        } else {
            StrategyName::Link
        };
        out.push(ToolchainChoice { id, strategy });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

/// Resolve strategy for a recipe (wizard default rules).
///
/// 1. `recipe.default_strategy` if set and defined  
/// 2. Heuristic by id / summary (cache → share, version-manager-ish → link)  
/// 3. Prefer link > share > isolate among defined strategies
pub fn default_strategy_for(recipe: &Recipe) -> StrategyName {
    if let Ok(s) = resolve_strategy(recipe, recipe.default_strategy) {
        return s;
    }
    let id = recipe.id.to_ascii_lowercase();
    let summary = recipe.summary.to_ascii_lowercase();
    let cache_hint = id.contains("maven")
        || id.contains("gradle")
        || id.contains("npm")
        || id.contains("pnpm")
        || id.contains("bun")
        || id.contains("pip")
        || id.contains("nuget")
        || id.contains("m2")
        || summary.contains("cache");
    let vm_hint = id.contains("nvm")
        || id.contains("sdkman")
        || id.contains("pyenv")
        || id.contains("rustup")
        || id.contains("mise")
        || id.contains("asdf")
        || summary.contains("version manager");
    // Caches prefer share; everything else (including version managers) prefers link.
    let preferred = if cache_hint {
        StrategyName::Share
    } else {
        let _ = vm_hint; // retained for docs / future differentiation
        StrategyName::Link
    };
    if recipe.strategies.contains_key(&preferred) {
        return preferred;
    }
    resolve_strategy(recipe, None).unwrap_or(StrategyName::Isolate)
}

/// Build toolchain choices for detected (found) recipes using defaults.
pub fn tools_from_detect(
    reg: &RecipeRegistry,
    found_ids: &[String],
) -> Result<Vec<ToolchainChoice>> {
    let ctx = crate::filter::RunContext::from_cmd(&[]);
    let mut out = Vec::new();
    for id in found_ids {
        let recipe = reg.resolve(id, &ctx)?;
        out.push(ToolchainChoice {
            id: recipe.id.clone(),
            strategy: default_strategy_for(recipe),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Bundle expansion
// ---------------------------------------------------------------------------

/// Expanded bundle template.
#[derive(Debug, Clone, Default)]
pub struct BundleExpansion {
    /// Suggested home (may be `@managed/…`).
    pub home: Option<String>,
    /// Profile list from the bundle.
    pub profiles: Vec<String>,
    /// Toolchain choices.
    pub tools: Vec<ToolchainChoice>,
    /// Bundle id for messages.
    pub id: String,
}

/// Load a bundle TOML (lenient) from an offline registry or path.
///
/// Accepts `registry:name:bundles/…`, `bundles/…` (search offline indexes),
/// or a filesystem path ending in `.toml`.
pub fn expand_bundle(spec: &str) -> Result<BundleExpansion> {
    let path = resolve_bundle_path(spec)?;
    let body = fs::read_to_string(&path).ctx(|| format!("reading bundle '{}'", path.display()))?;
    parse_bundle_toml(&body, &path.display().to_string())
}

fn resolve_bundle_path(spec: &str) -> Result<PathBuf> {
    let spec = spec.trim();
    // Bare path
    let as_path = PathBuf::from(spec);
    if as_path.is_file() {
        return Ok(as_path);
    }
    // registry:<name>:<id> or official:bundles/… or bundles/…
    let id = if let Some(rest) = spec.strip_prefix("registry:") {
        // registry:name:id  or registry:trust:name:id — take last two segments as name/id if 3+
        let parts: Vec<&str> = rest.split(':').collect();
        match parts.as_slice() {
            [_, id] => (*id).to_string(),
            [_, _, id] => (*id).to_string(),
            [id] => (*id).to_string(),
            _ => rest.to_string(),
        }
    } else if let Some((reg, id)) = spec.split_once(':') {
        // official:bundles/polyglot-agent
        let _ = reg;
        id.to_string()
    } else {
        spec.to_string()
    };

    // Search offline registry roots' index files.
    let registries = crate::registry::load_registries_from_config().unwrap_or_default();
    let cache = crate::registry::default_cache_root();
    let lock = crate::registry::Lockfile::load(&crate::registry::discover_lockfile_path())
        .unwrap_or_default();
    for (name, rspec) in &registries {
        if let Ok(src) = crate::registry::open_offline(name, rspec, &cache, &lock) {
            if let Some(entry) = src.index().get(&id) {
                let p = src.root().unwrap().join(&entry.file);
                if p.is_file() {
                    return Ok(p);
                }
            }
            // Fallback: recipes/<id>.toml
            if let Some(root) = src.root() {
                let candidates = [
                    root.join(format!("{id}.toml")),
                    root.join("recipes").join(format!("{id}.toml")),
                    root.join(format!("recipes/{id}.toml")),
                ];
                for c in candidates {
                    if c.is_file() {
                        return Ok(c);
                    }
                }
            }
        }
    }
    Err(Error::Message(format!(
        "bundle '{spec}' not found offline (run `@registry update` or pass a .toml path)"
    )))
}

fn parse_bundle_toml(body: &str, source: &str) -> Result<BundleExpansion> {
    let value: toml::Value = toml::from_str(body)
        .map_err(|e| Error::Message(format!("parsing bundle {source}: {e}")))?;
    let table = value
        .as_table()
        .ok_or_else(|| Error::Message(format!("bundle {source}: root must be a table")))?;
    let id = table
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(source)
        .to_string();
    let kind = table
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("bundle");
    if kind != "bundle" {
        return Err(Error::Message(format!(
            "bundle {source}: kind = {kind:?} (expected \"bundle\")"
        )));
    }
    let home = table
        .get("home")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let profiles = table
        .get("profiles")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut tools = Vec::new();
    // recipes = ["toolchains/nvm", …] without strategies
    if let Some(arr) = table.get("recipes").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(rid) = v.as_str() {
                tools.push(ToolchainChoice {
                    id: normalize_recipe_id(rid),
                    strategy: StrategyName::Link,
                });
            }
        }
    }
    // [toolchains] nvm = { strategy = "link" }  or  nvm = "link"
    if let Some(tc) = table.get("toolchains").and_then(|v| v.as_table()) {
        tools.clear(); // explicit toolchains table wins over bare recipes list strategies
                       // Re-seed ids from recipes list if toolchains only has strategies for subset
        let mut from_recipes: Vec<String> = table
            .get("recipes")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(normalize_recipe_id))
                    .collect()
            })
            .unwrap_or_default();
        for (key, val) in tc {
            let id = normalize_recipe_id(key);
            from_recipes.retain(|r| r != &id);
            let strategy = match val {
                toml::Value::String(s) => StrategyName::parse(s)?,
                toml::Value::Table(t) => {
                    let s = t.get("strategy").and_then(|v| v.as_str()).ok_or_else(|| {
                        Error::Message(format!(
                            "bundle {source}: toolchains.{key} missing strategy"
                        ))
                    })?;
                    StrategyName::parse(s)?
                }
                _ => {
                    return Err(Error::Message(format!(
                        "bundle {source}: toolchains.{key} must be a string or table"
                    )));
                }
            };
            tools.push(ToolchainChoice { id, strategy });
        }
        for rid in from_recipes {
            tools.push(ToolchainChoice {
                id: rid,
                strategy: StrategyName::Link,
            });
        }
    }
    tools.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(BundleExpansion {
        home,
        profiles,
        tools,
        id,
    })
}

// ---------------------------------------------------------------------------
// Managed hash + state
// ---------------------------------------------------------------------------

/// Canonical hash of managed toolchain choices (order-independent).
pub fn managed_hash(tools: &[ToolchainChoice]) -> String {
    let mut lines: Vec<String> = tools
        .iter()
        .map(|t| format!("{}={}\n", t.id, t.strategy.as_str()))
        .collect();
    lines.sort();
    sha256_hex(lines.join("").as_bytes())
}

/// Extract managed hash from an existing cage body (toolchains tables).
pub fn managed_hash_from_body(body: &str) -> Result<String> {
    let doc = body
        .parse::<DocumentMut>()
        .map_err(|e| Error::Message(format!("parsing cage TOML for hash: {e}")))?;
    let tools = toolchains_from_doc(&doc)?;
    Ok(managed_hash(&tools))
}

fn toolchains_from_doc(doc: &DocumentMut) -> Result<Vec<ToolchainChoice>> {
    let mut tools = Vec::new();
    let Some(tc) = doc.get("toolchains").and_then(|i| i.as_table()) else {
        return Ok(tools);
    };
    for (key, item) in tc.iter() {
        let strategy = item
            .get("strategy")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::Message(format!("cage toolchains.{key}: missing strategy string"))
            })?;
        tools.push(ToolchainChoice::new(key, strategy)?);
    }
    tools.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(tools)
}

/// Wizard state file (`state.toml`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WizardState {
    /// Per-cage records.
    #[serde(default)]
    pub cages: BTreeMap<String, CageStateEntry>,
}

/// One cage's last wizard write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CageStateEntry {
    /// Absolute path last written.
    pub path: String,
    /// Managed-section content hash.
    pub managed_hash: String,
}

/// Default path: `$XDG_CONFIG_HOME/isol8/state.toml` or `~/.config/isol8/state.toml`.
pub fn state_path() -> PathBuf {
    crate::cage::user_cages_dir()
        .map(|p| p.parent().unwrap_or(p.as_path()).join("state.toml"))
        .unwrap_or_else(|| PathBuf::from("isol8-state.toml"))
}

/// Load wizard state (empty if missing).
pub fn load_state(path: &Path) -> Result<WizardState> {
    if !path.is_file() {
        return Ok(WizardState::default());
    }
    let body = fs::read_to_string(path).ctx(|| format!("reading state '{}'", path.display()))?;
    toml::from_str(&body).ctx(|| format!("parsing state '{}'", path.display()))
}

/// Save wizard state.
pub fn save_state(path: &Path, state: &WizardState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ctx(|| format!("creating '{}'", parent.display()))?;
    }
    let body = toml::to_string_pretty(state)
        .map_err(|e| Error::Message(format!("serializing state: {e}")))?;
    let header = "# isol8 wizard state — managed-section hashes (Phase 8)\n\
                  # Safe to delete; the next @cage edit will re-seed entries.\n\n";
    fs::write(path, format!("{header}{body}")).ctx(|| format!("writing state '{}'", path.display()))
}

/// Check whether rewriting managed sections would clobber hand edits.
pub fn check_drift(name: &str, cage_path: &Path, state: &WizardState) -> Result<DriftStatus> {
    if !cage_path.is_file() {
        return Ok(DriftStatus::Clean);
    }
    let body =
        fs::read_to_string(cage_path).ctx(|| format!("reading cage '{}'", cage_path.display()))?;
    let actual = managed_hash_from_body(&body)?;
    match state.cages.get(name) {
        None => Ok(DriftStatus::UnknownExisting),
        Some(ent) if ent.managed_hash == actual => Ok(DriftStatus::Unchanged),
        Some(ent) => Ok(DriftStatus::Drift {
            path: PathBuf::from(&ent.path),
            expected: ent.managed_hash.clone(),
            actual,
        }),
    }
}

// ---------------------------------------------------------------------------
// Render + write
// ---------------------------------------------------------------------------

/// Render cage TOML for a request (does not write).
pub fn render(req: &WizardRequest) -> Result<WizardResult> {
    let home = normalize_home(&req.name, &req.home)?;
    let mut warnings = Vec::new();
    // Absolute custom homes are allowed but less portable than @managed/<id>.
    if home != "inherit"
        && home != "ephemeral"
        && !home.starts_with("@managed/")
        && !home.starts_with('~')
        && Path::new(&home).is_absolute()
    {
        warnings.push(format!(
            "home = {home:?} is an absolute path (less portable than @managed/{})",
            req.name
        ));
    }
    if home == "inherit" && !req.tools.is_empty() {
        warnings.push(
            "home = inherit with toolchains: strategy grants still apply; \
             materialization under ~ targets the real home"
                .into(),
        );
    }

    let hash = managed_hash(&req.tools);
    let body = if let Some(existing) = &req.existing_path {
        if existing.is_file() {
            let prev =
                fs::read_to_string(existing).ctx(|| format!("reading '{}'", existing.display()))?;
            rewrite_managed(
                &prev,
                &req.name,
                &home,
                &req.profiles,
                &req.tools,
                &req.dirs,
            )?
        } else {
            render_fresh(&req.name, &home, &req.profiles, &req.tools, &req.dirs)
        }
    } else {
        render_fresh(&req.name, &home, &req.profiles, &req.tools, &req.dirs)
    };

    let path = dest_path(req)?;
    Ok(WizardResult {
        path,
        body,
        managed_hash: hash,
        warnings,
    })
}

fn dest_path(req: &WizardRequest) -> Result<PathBuf> {
    if let Some(p) = &req.existing_path {
        return Ok(p.clone());
    }
    let base = match &req.out_dir {
        Some(d) => d.clone(),
        None => crate::cage::user_cages_dir().ok_or_else(|| {
            Error::Message(
                "cannot determine cages dir (set HOME / XDG_CONFIG_HOME or pass --path)".into(),
            )
        })?,
    };
    Ok(base.join(format!("{}.toml", req.name)))
}

/// Write the rendered cage and update state.
pub fn apply(req: &WizardRequest, state_file: &Path) -> Result<WizardResult> {
    if req.name.is_empty() || req.name.contains('/') || req.name.contains('\\') {
        return Err(Error::Message(
            "cage name must be a non-empty bare identifier (no path separators)".into(),
        ));
    }
    let rendered = render(req)?;
    let path = &rendered.path;

    if path.exists() && !req.force && req.existing_path.is_none() {
        return Err(Error::Message(format!(
            "cage already exists at {} (use --force or `@cage edit`)",
            path.display()
        )));
    }

    if path.exists() && !req.force {
        let mut state = load_state(state_file)?;
        match check_drift(&req.name, path, &state)? {
            DriftStatus::Drift {
                expected, actual, ..
            } => {
                return Err(Error::Message(format!(
                    "cage '{}': managed [toolchains.*] were hand-edited \
                     (state hash {expected:.12}…, file {actual:.12}…). \
                     Re-run with --force to overwrite, or restore the managed sections",
                    req.name
                )));
            }
            DriftStatus::UnknownExisting if req.existing_path.is_some() => {
                // edit without prior state: refuse unless force
                return Err(Error::Message(format!(
                    "cage '{}': exists but has no wizard state hash — \
                     refusing to rewrite toolchains (pass --force to take ownership)",
                    req.name
                )));
            }
            _ => {}
        }
        let _ = &mut state;
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ctx(|| format!("creating '{}'", parent.display()))?;
    }
    fs::write(path, &rendered.body).ctx(|| format!("writing cage '{}'", path.display()))?;

    let mut state = load_state(state_file)?;
    state.cages.insert(
        req.name.clone(),
        CageStateEntry {
            path: path.display().to_string(),
            managed_hash: rendered.managed_hash.clone(),
        },
    );
    save_state(state_file, &state)?;

    Ok(rendered)
}

fn render_fresh(
    name: &str,
    home: &str,
    profiles: &[String],
    tools: &[ToolchainChoice],
    dirs: &[(String, Access)],
) -> String {
    let mut out = String::new();
    out.push_str("# isol8 cage — generated by `isol8 @cage new` / `@cage edit` (Phase 8)\n");
    out.push_str(&format!(
        "schema = 1\nname = \"{name}\"\nhome = \"{home}\"\n\n"
    ));

    if profiles.is_empty() {
        out.push_str("# profiles = [] → config default_profiles apply at run time\n");
        out.push_str("profiles = []\n\n");
    } else {
        out.push_str("profiles = [\n");
        for p in profiles {
            out.push_str(&format!("  \"{p}\",\n"));
        }
        out.push_str("]\n\n");
    }

    if tools.is_empty() {
        out.push_str("# no toolchains — add with `@cage edit` or [toolchains.*]\n");
    } else {
        for tc in tools {
            let short = tc.id.strip_prefix("toolchains/").unwrap_or(tc.id.as_str());
            out.push_str(&format!("# {MANAGED_MARKER} — rewritten by `@cage edit`\n"));
            out.push_str(&format!(
                "[toolchains.{short}]\nstrategy = \"{}\"\n\n",
                tc.strategy.as_str()
            ));
        }
    }

    if dirs.is_empty() {
        out.push_str("# user-owned dirs (wizard never rewrites [[dirs]]):\n");
        out.push_str("# [[dirs]]\n# path = \"~/project\"\n# access = \"rw\"\n");
    } else {
        out.push_str("# user-owned dirs (wizard preserves these on edit):\n");
        for (path, access) in dirs {
            let a = match access {
                Access::Ro => "ro",
                Access::Rw => "rw",
                _ => "rw",
            };
            out.push_str(&format!(
                "[[dirs]]\npath = \"{path}\"\naccess = \"{a}\"\n\n"
            ));
        }
    }
    out
}

/// Rewrite managed toolchains in an existing document; preserve dirs and unknown keys.
fn rewrite_managed(
    existing: &str,
    name: &str,
    home: &str,
    profiles: &[String],
    tools: &[ToolchainChoice],
    dirs: &[(String, Access)],
) -> Result<String> {
    let mut doc = existing
        .parse::<DocumentMut>()
        .map_err(|e| Error::Message(format!("parsing existing cage: {e}")))?;

    doc["schema"] = value(1);
    doc["name"] = value(name);
    doc["home"] = value(home);

    // profiles: replace only when the request provides a non-empty list
    if !profiles.is_empty() {
        let mut arr = toml_edit::Array::new();
        for p in profiles {
            arr.push(p.as_str());
        }
        doc["profiles"] = Item::Value(toml_edit::Value::Array(arr));
    }

    // Replace toolchains table wholesale (managed).
    let mut tc_table = Table::new();
    tc_table.set_implicit(true);
    for t in tools {
        let short = t.id.strip_prefix("toolchains/").unwrap_or(t.id.as_str());
        let mut entry = Table::new();
        entry["strategy"] = value(t.strategy.as_str());
        // Decor: managed marker as comment above key is limited in toml_edit;
        // we set a prefix comment on the table key via decor when possible.
        tc_table[short] = Item::Table(entry);
    }
    if tools.is_empty() {
        doc.as_table_mut().remove("toolchains");
    } else {
        // Prefix comment on the toolchains table
        let mut item = Item::Table(tc_table);
        if let Some(t) = item.as_table_mut() {
            t.decor_mut().set_prefix(format!(
                "\n# {MANAGED_MARKER} — [toolchains.*] rewritten by `@cage edit`\n"
            ));
        }
        doc["toolchains"] = item;
    }

    // dirs: only inject when the existing doc has none and the request has some
    let has_dirs = doc.get("dirs").map(|i| !i.is_none()).unwrap_or(false);
    if !has_dirs && !dirs.is_empty() {
        let mut arr = toml_edit::ArrayOfTables::new();
        for (path, access) in dirs {
            let mut t = Table::new();
            t["path"] = value(path.as_str());
            t["access"] = value(match access {
                Access::Ro => "ro",
                Access::Rw => "rw",
                _ => "rw",
            });
            arr.push(t);
        }
        doc["dirs"] = Item::ArrayOfTables(arr);
    }

    Ok(doc.to_string())
}

/// Format a short preview of effective rw grants outside replaced home (heuristic).
pub fn preview_security_notes(tools: &[ToolchainChoice], reg: &RecipeRegistry) -> Vec<String> {
    let ctx = crate::filter::RunContext::from_cmd(&[]);
    let mut notes = Vec::new();
    for tc in tools {
        let Ok(recipe) = reg.resolve(&tc.id, &ctx) else {
            continue;
        };
        let Ok(contrib) = reg.compile(tc, &ctx) else {
            continue;
        };
        for g in &contrib.paths {
            if matches!(g.access, Access::Rw) && g.path.starts_with("#HOME") {
                notes.push(format!(
                    "rw outside replaced home: {} ({}, strategy {})",
                    g.path,
                    recipe.id,
                    tc.strategy.as_str()
                ));
            }
        }
        let _ = recipe;
    }
    notes
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tmp() -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "isol8-wiz-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn normalize_home_managed() {
        assert_eq!(normalize_home("work", "managed").unwrap(), "@managed/work");
        assert_eq!(normalize_home("work", "inherit").unwrap(), "inherit");
        assert_eq!(
            normalize_home("work", "@managed/custom").unwrap(),
            "@managed/custom"
        );
    }

    #[test]
    fn parse_tools_with_strategies() {
        let tools = parse_tools_list("nvm,cargo:share,maven:isolate", None).unwrap();
        assert_eq!(tools.len(), 3);
        let cargo = tools.iter().find(|t| t.id == "toolchains/cargo").unwrap();
        assert_eq!(cargo.strategy, StrategyName::Share);
        let nvm = tools.iter().find(|t| t.id == "toolchains/nvm").unwrap();
        assert_eq!(nvm.strategy, StrategyName::Link);
    }

    #[test]
    fn render_and_hash_stable() {
        let req = WizardRequest {
            name: "work".into(),
            home: "managed".into(),
            tools: vec![
                ToolchainChoice {
                    id: "toolchains/nvm".into(),
                    strategy: StrategyName::Link,
                },
                ToolchainChoice {
                    id: "toolchains/cargo".into(),
                    strategy: StrategyName::Share,
                },
            ],
            dirs: vec![("~/proj".into(), Access::Rw)],
            profiles: vec!["base".into()],
            out_dir: Some(tmp()),
            force: false,
            existing_path: None,
        };
        let r = render(&req).unwrap();
        assert!(r.body.contains("name = \"work\""));
        assert!(r.body.contains("home = \"@managed/work\""));
        assert!(r.body.contains("[toolchains.nvm]"));
        assert!(r.body.contains("strategy = \"link\""));
        assert!(r.body.contains("[toolchains.cargo]"));
        assert!(r.body.contains("[[dirs]]"));
        assert!(r.body.contains(MANAGED_MARKER));
        assert_eq!(r.managed_hash, managed_hash(&req.tools));
        let _ = fs::remove_dir_all(req.out_dir.unwrap());
    }

    #[test]
    fn apply_writes_and_state() {
        let dir = tmp();
        let state = dir.join("state.toml");
        let req = WizardRequest {
            name: "demo".into(),
            home: "inherit".into(),
            tools: vec![ToolchainChoice {
                id: "toolchains/nvm".into(),
                strategy: StrategyName::Link,
            }],
            dirs: vec![],
            profiles: vec![],
            out_dir: Some(dir.clone()),
            force: false,
            existing_path: None,
        };
        let r = apply(&req, &state).unwrap();
        assert!(r.path.is_file());
        let st = load_state(&state).unwrap();
        assert_eq!(st.cages["demo"].managed_hash, r.managed_hash);

        // Second write without force fails (exists)
        let err = apply(&req, &state).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");

        // force overwrites
        let mut req2 = req.clone();
        req2.force = true;
        req2.tools = vec![ToolchainChoice {
            id: "toolchains/cargo".into(),
            strategy: StrategyName::Link,
        }];
        let r2 = apply(&req2, &state).unwrap();
        assert!(r2.body.contains("[toolchains.cargo]"));
        assert!(!r2.body.contains("[toolchains.nvm]"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn drift_detection() {
        let dir = tmp();
        let state_path = dir.join("state.toml");
        let req = WizardRequest {
            name: "d".into(),
            home: "inherit".into(),
            tools: vec![ToolchainChoice {
                id: "toolchains/nvm".into(),
                strategy: StrategyName::Link,
            }],
            dirs: vec![],
            profiles: vec![],
            out_dir: Some(dir.clone()),
            force: false,
            existing_path: None,
        };
        let r = apply(&req, &state_path).unwrap();
        let st = load_state(&state_path).unwrap();
        assert_eq!(
            check_drift("d", &r.path, &st).unwrap(),
            DriftStatus::Unchanged
        );

        // Hand-edit toolchains
        let mut body = fs::read_to_string(&r.path).unwrap();
        body = body.replace("strategy = \"link\"", "strategy = \"share\"");
        fs::write(&r.path, body).unwrap();
        match check_drift("d", &r.path, &st).unwrap() {
            DriftStatus::Drift { .. } => {}
            other => panic!("expected Drift, got {other:?}"),
        }

        // edit without force fails
        let mut edit = req.clone();
        edit.existing_path = Some(r.path.clone());
        edit.tools = vec![ToolchainChoice {
            id: "toolchains/maven".into(),
            strategy: StrategyName::Share,
        }];
        let err = apply(&edit, &state_path).unwrap_err();
        assert!(err.to_string().contains("hand-edited"), "{err}");

        edit.force = true;
        let r2 = apply(&edit, &state_path).unwrap();
        assert!(r2.body.contains("maven"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewrite_preserves_user_dirs() {
        let existing = r#"
schema = 1
name = "work"
home = "inherit"
profiles = []

[toolchains.nvm]
strategy = "link"

# user comment
[[dirs]]
path = "~/keep-me"
access = "rw"
"#;
        let tools = vec![ToolchainChoice {
            id: "toolchains/cargo".into(),
            strategy: StrategyName::Link,
        }];
        let out = rewrite_managed(existing, "work", "@managed/work", &[], &tools, &[]).unwrap();
        assert!(out.contains("keep-me"), "dirs preserved: {out}");
        assert!(out.contains("cargo"));
        assert!(!out.contains("toolchains.nvm") && !out.contains("[toolchains.nvm]"));
        assert!(out.contains("@managed/work"));
    }

    #[test]
    fn parse_bundle_minimal() {
        let body = r#"
schema = 1
id = "bundles/demo"
kind = "bundle"
home = "@managed/demo"
profiles = ["base"]
recipes = ["toolchains/nvm", "toolchains/cargo"]
[toolchains]
nvm = { strategy = "link" }
cargo = { strategy = "share" }
"#;
        let b = parse_bundle_toml(body, "demo").unwrap();
        assert_eq!(b.id, "bundles/demo");
        assert_eq!(b.home.as_deref(), Some("@managed/demo"));
        assert_eq!(b.profiles, vec!["base"]);
        assert_eq!(b.tools.len(), 2);
        let cargo = b.tools.iter().find(|t| t.id.contains("cargo")).unwrap();
        assert_eq!(cargo.strategy, StrategyName::Share);
    }

    #[test]
    fn default_strategy_uses_recipe_field() {
        let reg = RecipeRegistry::load(&[]).unwrap();
        let ctx = crate::filter::RunContext::from_cmd(&[]);
        if let Ok(nvm) = reg.resolve("toolchains/nvm", &ctx) {
            let s = default_strategy_for(nvm);
            assert_eq!(s, StrategyName::Link); // nvm default_strategy = link
        }
    }
}
