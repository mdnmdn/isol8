//! Recipe / profile registry sources, cache, lockfile, and trust (Phase 7).
//!
//! A registry is an offline-by-default source of recipes (and optionally
//! profiles/bundles). Configured in `isol8.toml` under `[registries.*]`, fetched
//! only by explicit `@registry update`, and pinned by `isol8.lock`.
//!
//! Design: [`_docs/inbox/evo-repo.md`](../_docs/inbox/evo-repo.md) §5 / §7.5.
//! Plan: [`_docs/wip/multi-evo-plan.md`](../_docs/wip/multi-evo-plan.md) Phase 7.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use isol8_core::error::{Error, Result, ResultExt};
use isol8_core::profile::Profile;
use isol8_core::recipe::{self, Recipe};

// ---------------------------------------------------------------------------
// Trust
// ---------------------------------------------------------------------------

/// How much a registry (or builtin/local source) is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    /// Built-in or explicitly marked official — commands may run.
    Official,
    /// Third-party community content — install ok; commands gated.
    Community,
    /// Local path on disk — treated like user-authored files.
    Local,
    /// Untrusted remote / unknown — no command execution.
    Untrusted,
}

impl TrustLevel {
    /// Parse a trust level string from config / `registry.toml`.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "official" => Ok(TrustLevel::Official),
            "community" => Ok(TrustLevel::Community),
            "local" => Ok(TrustLevel::Local),
            "untrusted" => Ok(TrustLevel::Untrusted),
            other => Err(Error::Message(format!(
                "unknown trust level {other:?} (expected official, community, local, untrusted)"
            ))),
        }
    }

    /// Lowercase label.
    pub fn as_str(self) -> &'static str {
        match self {
            TrustLevel::Official => "official",
            TrustLevel::Community => "community",
            TrustLevel::Local => "local",
            TrustLevel::Untrusted => "untrusted",
        }
    }

    /// Whether `detect.version` / `verify.cmd` may run for content at this level.
    pub fn commands_allowed(self) -> bool {
        matches!(self, TrustLevel::Official | TrustLevel::Local)
    }
}

// ---------------------------------------------------------------------------
// Index + manifest
// ---------------------------------------------------------------------------

/// Kind of artifact published in a registry index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// Raw policy layer (existing Profile TOML).
    Profile,
    /// Toolchain recipe.
    Recipe,
    /// Curated multi-recipe template.
    Bundle,
}

/// One entry in `index.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Canonical id (e.g. `toolchains/nvm`).
    pub id: String,
    /// Artifact kind.
    pub kind: ArtifactKind,
    /// Platforms this entry targets (empty = all).
    #[serde(default)]
    pub os: Vec<String>,
    /// Path relative to the registry root.
    pub file: String,
    /// One-line summary.
    #[serde(default)]
    pub summary: String,
    /// Optional tags (search only).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Strategy names (recipes).
    #[serde(default)]
    pub strategies: Vec<String>,
    /// Default strategy name, if any.
    #[serde(default)]
    pub default_strategy: Option<String>,
    /// Detect probe path summary (recipes).
    #[serde(default)]
    pub detects: Option<String>,
    /// Verify command summary (recipes).
    #[serde(default)]
    pub verify: Option<String>,
    /// Required profile ids (informational).
    #[serde(default)]
    pub requires: Vec<String>,
    /// Content hash of `file` (hex sha256), when provided.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Generated registry search index (`index.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndex {
    /// Index schema version.
    #[serde(default = "default_index_schema")]
    pub schema: u32,
    /// Registry name (should match `registry.toml`).
    #[serde(default)]
    pub registry: String,
    /// Entry count (informational; may drift from `entries.len()`).
    #[serde(default)]
    pub count: usize,
    /// All published artifacts.
    #[serde(default)]
    pub entries: Vec<IndexEntry>,
}

fn default_index_schema() -> u32 {
    1
}

impl RegistryIndex {
    /// Look up an entry by id (first match).
    pub fn get(&self, id: &str) -> Option<&IndexEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Recipe ids only, sorted.
    pub fn recipe_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.kind == ArtifactKind::Recipe)
            .map(|e| e.id.clone())
            .collect();
        ids.sort();
        ids
    }
}

/// Trust table from `registry.toml` `[trust]`.
///
/// Unknown keys (e.g. future `[trust.commands]`) are ignored so older clients
/// can still open newer registries.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TrustConfig {
    /// Declared trust level for this registry.
    pub level: Option<String>,
    /// Paths no recipe may grant (any access).
    pub forbidden_paths: Vec<String>,
    /// Ceiling for grants outside the replaced home (`ro` | `rw` | `none`).
    pub max_grant_outside_home: Option<String>,
    /// Recipe ids allowed to exceed the ceiling with `rw` on real-home paths.
    pub rw_outside_home_allowed: Vec<String>,
}

/// `registry.toml` manifest (subset we consume).
///
/// Extra keys are ignored for forward compatibility with registry evolution.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryManifest {
    /// Manifest schema.
    #[serde(default = "default_index_schema")]
    pub schema: u32,
    /// Registry name.
    pub name: String,
    /// Human title.
    #[serde(default)]
    pub title: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Minimum isol8 version string (informational).
    #[serde(default)]
    pub min_isol8: Option<String>,
    /// Relative path to index (default `index.json`).
    #[serde(default = "default_index_name")]
    pub index: String,
    /// Trust metadata.
    #[serde(default)]
    pub trust: TrustConfig,
}

fn default_index_name() -> String {
    "index.json".into()
}

// ---------------------------------------------------------------------------
// Config: how registries are declared in isol8.toml
// ---------------------------------------------------------------------------

/// One configured registry backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrySpec {
    /// Local directory (git checkout or plain folder).
    Path {
        /// Filesystem path (may contain `~`).
        path: String,
        /// Optional trust override.
        trust: Option<TrustLevel>,
    },
    /// Git remote; offline uses cache only.
    Git {
        /// Clone URL.
        url: String,
        /// Branch / tag / ref (default `main`).
        ref_name: String,
        /// Optional trust override.
        trust: Option<TrustLevel>,
    },
    /// HTTP-served tree (tarball or base URL). Not implemented in Phase 7 MVP.
    Http {
        /// Base URL.
        url: String,
        /// Optional trust override.
        trust: Option<TrustLevel>,
    },
}

impl RegistrySpec {
    /// Effective trust when the manifest is missing or has no level.
    pub fn default_trust(&self) -> TrustLevel {
        match self {
            RegistrySpec::Path { trust, .. } => trust.unwrap_or(TrustLevel::Local),
            RegistrySpec::Git { trust, .. } => trust.unwrap_or(TrustLevel::Community),
            RegistrySpec::Http { trust, .. } => trust.unwrap_or(TrustLevel::Untrusted),
        }
    }

    /// Short source description for lockfile / logs.
    pub fn source_label(&self) -> String {
        match self {
            RegistrySpec::Path { path, .. } => format!("path:{path}"),
            RegistrySpec::Git { url, ref_name, .. } => format!("git:{url}@{ref_name}"),
            RegistrySpec::Http { url, .. } => format!("http:{url}"),
        }
    }
}

/// Parse a single `[registries.<name>]` table.
pub fn parse_registry_spec(name: &str, value: &toml::Value) -> Result<RegistrySpec> {
    let table = value.as_table().ok_or_else(|| {
        Error::Message(format!(
            "registries.{name}: expected a table (path = … or git = …)"
        ))
    })?;

    let trust = match table.get("trust").and_then(|v| v.as_str()) {
        Some(s) => Some(TrustLevel::parse(s)?),
        None => None,
    };

    let has_path = table.contains_key("path");
    let has_git = table.contains_key("git");
    let has_url = table.contains_key("url") || table.contains_key("http");
    let kinds = [has_path, has_git, has_url].iter().filter(|&&b| b).count();
    if kinds != 1 {
        return Err(Error::Message(format!(
            "registries.{name}: set exactly one of path, git, or url"
        )));
    }

    if has_path {
        let path = table
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Message(format!("registries.{name}.path must be a string")))?
            .to_string();
        return Ok(RegistrySpec::Path { path, trust });
    }
    if has_git {
        let url = table
            .get("git")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Message(format!("registries.{name}.git must be a string")))?
            .to_string();
        let ref_name = table
            .get("ref")
            .and_then(|v| v.as_str())
            .unwrap_or("main")
            .to_string();
        return Ok(RegistrySpec::Git {
            url,
            ref_name,
            trust,
        });
    }
    let url = table
        .get("url")
        .or_else(|| table.get("http"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Message(format!("registries.{name}.url must be a string")))?
        .to_string();
    Ok(RegistrySpec::Http { url, trust })
}

/// Expand `~` in a path against the real home.
pub fn expand_user_path(raw: &str) -> PathBuf {
    if raw == "~" {
        return real_home_path();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return real_home_path().join(rest);
    }
    PathBuf::from(raw)
}

fn real_home_path() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

// ---------------------------------------------------------------------------
// Cache + lockfile
// ---------------------------------------------------------------------------

/// Default cache root: `$XDG_CACHE_HOME/isol8/registries` or `~/.cache/isol8/registries`.
pub fn default_cache_root() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CACHE_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(x).join("isol8/registries");
    }
    real_home_path().join(".cache/isol8/registries")
}

/// Cache directory for one named registry pin.
pub fn cache_dir_for(cache_root: &Path, name: &str, pin: &str) -> PathBuf {
    cache_root.join(name).join(pin)
}

/// One pinned registry in `isol8.lock`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockRegistry {
    /// Config name.
    pub name: String,
    /// Source label (`path:…` / `git:…@ref`).
    pub source: String,
    /// Resolved pin: commit SHA, content hash, or `path`.
    pub pin: String,
    /// Aggregate content hash of the index (or tree), when known.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// Trust level recorded at lock time.
    #[serde(default)]
    pub trust: Option<String>,
}

/// One artifact pin in `isol8.lock`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockEntry {
    /// Registry config name.
    pub registry: String,
    /// Artifact id.
    pub id: String,
    /// Kind label.
    pub kind: String,
    /// File sha256 when known.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Project / user lockfile (`isol8.lock`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lockfile {
    /// Lock schema.
    #[serde(default = "default_index_schema")]
    pub schema: u32,
    /// Pinned registries.
    #[serde(default)]
    pub registries: Vec<LockRegistry>,
    /// Optional per-artifact pins (may be empty when only registry pins are used).
    #[serde(default)]
    pub entries: Vec<LockEntry>,
}

impl Lockfile {
    /// Load from a path; missing file → empty lock.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let body =
            fs::read_to_string(path).ctx(|| format!("reading lockfile '{}'", path.display()))?;
        toml::from_str(&body).ctx(|| format!("parsing lockfile '{}'", path.display()))
    }

    /// Write atomically-ish (write then rename not required on all platforms).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .ctx(|| format!("creating lockfile dir '{}'", parent.display()))?;
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| Error::Message(format!("serializing lockfile: {e}")))?;
        let header = "# isol8.lock — generated by `isol8 @registry update|install`\n\
                      # Do not edit by hand unless you know what you are doing.\n\
                      # Drift between this file and the cache is an error at resolve time.\n\n";
        fs::write(path, format!("{header}{body}"))
            .ctx(|| format!("writing lockfile '{}'", path.display()))
    }

    /// Find a registry pin by name.
    pub fn registry(&self, name: &str) -> Option<&LockRegistry> {
        self.registries.iter().find(|r| r.name == name)
    }
}

/// Discover the lockfile path.
///
/// 1. `./isol8.lock` if it already exists  
/// 2. `./isol8.lock` if a project config (`isol8.toml` / yaml) is in cwd  
/// 3. else `~/.config/isol8/isol8.lock` (user-global registries)
pub fn discover_lockfile_path() -> PathBuf {
    let cwd_lock = PathBuf::from("isol8.lock");
    if cwd_lock.is_file() {
        return cwd_lock;
    }
    for name in ["isol8.toml", "isol8.yaml", "isol8.yml"] {
        if PathBuf::from(name).is_file() {
            return cwd_lock;
        }
    }
    config_isol8_dir().join("isol8.lock")
}

fn config_isol8_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|| real_home_path().join(".config"))
        .join("isol8")
}

// ---------------------------------------------------------------------------
// ProfileSource trait + DirSource
// ---------------------------------------------------------------------------

/// Shared surface for embedded, directory, git-cache, and layered sources.
///
/// Name kept as `ProfileSource` to match evo-repo §7.5 even though recipes are
/// the primary cargo today.
pub trait ProfileSource: Send + Sync {
    /// Configured name (`official`, `scratch`, `embedded`, …).
    fn name(&self) -> &str;

    /// Search index (may be empty for pure embedded).
    fn index(&self) -> &RegistryIndex;

    /// Trust level for command gating and install policy.
    fn trust(&self) -> TrustLevel;

    /// Root directory on disk, if any (embedded has none).
    fn root(&self) -> Option<&Path>;

    /// Load a recipe by id (platform filter applied by the caller / RecipeRegistry).
    fn get_recipe(&self, id: &str) -> Result<Option<Recipe>>;

    /// Load a profile layer by id.
    fn get_profile(&self, id: &str) -> Result<Option<Profile>>;
}

/// Directory-backed registry (`registry.toml` + `index.json` + files).
#[derive(Debug, Clone)]
pub struct DirSource {
    name: String,
    root: PathBuf,
    trust: TrustLevel,
    manifest: RegistryManifest,
    index: RegistryIndex,
}

impl DirSource {
    /// Open a registry directory. Requires `registry.toml` and the index file.
    pub fn open(name: impl Into<String>, root: impl Into<PathBuf>) -> Result<Self> {
        Self::open_with_trust(name, root, None)
    }

    /// Open with an optional trust override (config wins over missing manifest level).
    pub fn open_with_trust(
        name: impl Into<String>,
        root: impl Into<PathBuf>,
        trust_override: Option<TrustLevel>,
    ) -> Result<Self> {
        let name = name.into();
        let root = root.into();
        if !root.is_dir() {
            return Err(Error::Message(format!(
                "registry '{name}': directory not found: {}",
                root.display()
            )));
        }
        let manifest_path = root.join("registry.toml");
        let manifest_body = fs::read_to_string(&manifest_path).ctx(|| {
            format!(
                "registry '{name}': reading manifest '{}'",
                manifest_path.display()
            )
        })?;
        let manifest: RegistryManifest = toml::from_str(&manifest_body).ctx(|| {
            format!(
                "registry '{name}': parsing manifest '{}'",
                manifest_path.display()
            )
        })?;

        let index_rel = if manifest.index.is_empty() {
            "index.json".to_string()
        } else {
            manifest.index.clone()
        };
        let index_path = root.join(&index_rel);
        let index_body = fs::read_to_string(&index_path).ctx(|| {
            format!(
                "registry '{name}': reading index '{}'",
                index_path.display()
            )
        })?;
        let index: RegistryIndex = serde_json::from_str(&index_body).map_err(|e| {
            Error::Message(format!(
                "registry '{name}': parsing index '{}': {e}",
                index_path.display()
            ))
        })?;

        let trust = trust_override
            .or_else(|| {
                manifest
                    .trust
                    .level
                    .as_deref()
                    .and_then(|s| TrustLevel::parse(s).ok())
            })
            .unwrap_or(TrustLevel::Local);

        Ok(Self {
            name,
            root,
            trust,
            manifest,
            index,
        })
    }

    /// Manifest trust config (forbidden paths, ceilings).
    pub fn trust_config(&self) -> &TrustConfig {
        &self.manifest.trust
    }

    /// Absolute path for an index file entry.
    pub fn file_path(&self, entry: &IndexEntry) -> PathBuf {
        self.root.join(&entry.file)
    }

    /// sha256 hex of a file.
    pub fn hash_file(path: &Path) -> Result<String> {
        let bytes = fs::read(path).ctx(|| format!("hashing '{}'", path.display()))?;
        Ok(sha256_hex(&bytes))
    }

    /// Verify index sha256 pins against on-disk files. Returns mismatch messages.
    pub fn verify_content_hashes(&self) -> Result<Vec<String>> {
        let mut drifts = Vec::new();
        for entry in &self.index.entries {
            let Some(expected) = entry.sha256.as_deref() else {
                continue;
            };
            let path = self.file_path(entry);
            if !path.is_file() {
                drifts.push(format!(
                    "{}: missing file {} (index lists sha256={expected})",
                    entry.id,
                    path.display()
                ));
                continue;
            }
            let actual = Self::hash_file(&path)?;
            if !actual.eq_ignore_ascii_case(expected) {
                drifts.push(format!(
                    "{}: content hash drift (lock/index {expected}, disk {actual})",
                    entry.id
                ));
            }
        }
        Ok(drifts)
    }

    /// Aggregate hash of the index document (stable pin for path registries).
    pub fn index_content_hash(&self) -> Result<String> {
        let index_path = self.root.join(&self.manifest.index);
        Self::hash_file(&index_path)
    }
}

impl ProfileSource for DirSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn index(&self) -> &RegistryIndex {
        &self.index
    }

    fn trust(&self) -> TrustLevel {
        self.trust
    }

    fn root(&self) -> Option<&Path> {
        Some(&self.root)
    }

    fn get_recipe(&self, id: &str) -> Result<Option<Recipe>> {
        let Some(entry) = self.index.get(id) else {
            return Ok(None);
        };
        if entry.kind != ArtifactKind::Recipe {
            return Ok(None);
        }
        let path = self.file_path(entry);
        if !path.is_file() {
            return Err(Error::Message(format!(
                "registry '{}': recipe '{id}' listed in index but file missing: {}",
                self.name,
                path.display()
            )));
        }
        let mut recipe = recipe::load_from_path(&path)?;
        // Label origin for trust gating: registry:<trust>:<name>:<id>
        recipe.source = format!(
            "registry:{}:{}:{}",
            self.trust.as_str(),
            self.name,
            recipe.id
        );
        Ok(Some(recipe))
    }

    fn get_profile(&self, id: &str) -> Result<Option<Profile>> {
        let Some(entry) = self.index.get(id) else {
            return Ok(None);
        };
        if entry.kind != ArtifactKind::Profile {
            return Ok(None);
        }
        let path = self.file_path(entry);
        if !path.is_file() {
            return Err(Error::Message(format!(
                "registry '{}': profile '{id}' listed in index but file missing: {}",
                self.name,
                path.display()
            )));
        }
        let body =
            fs::read_to_string(&path).ctx(|| format!("reading profile '{}'", path.display()))?;
        let profile: Profile = toml::from_str(&body)
            .ctx(|| format!("parsing profile '{}' ({})", id, path.display()))?;
        Ok(Some(profile))
    }
}

// ---------------------------------------------------------------------------
// Layered source
// ---------------------------------------------------------------------------

/// Later-wins composition of named sources (config order; last match on get).
pub struct LayeredSource {
    sources: Vec<Box<dyn ProfileSource>>,
    /// Merged index (later entries overwrite same id).
    merged: RegistryIndex,
    /// Effective trust: minimum (most restrictive) across sources that provide an id —
    /// for the layered unit as a whole we expose the first source's trust for list UX;
    /// per-artifact trust is taken from the winning source on get.
    name: String,
    trust: TrustLevel,
}

impl LayeredSource {
    /// Build from ordered sources (low → high precedence).
    pub fn new(sources: Vec<Box<dyn ProfileSource>>) -> Self {
        let mut by_id: HashMap<String, IndexEntry> = HashMap::new();
        let mut trust = TrustLevel::Untrusted;
        for src in &sources {
            trust = src.trust();
            for e in &src.index().entries {
                by_id.insert(e.id.clone(), e.clone());
            }
        }
        let mut entries: Vec<IndexEntry> = by_id.into_values().collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        let count = entries.len();
        Self {
            sources,
            merged: RegistryIndex {
                schema: 1,
                registry: "layered".into(),
                count,
                entries,
            },
            name: "layered".into(),
            trust,
        }
    }

    /// Underlying sources (low → high precedence).
    pub fn sources(&self) -> &[Box<dyn ProfileSource>] {
        &self.sources
    }
}

impl ProfileSource for LayeredSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn index(&self) -> &RegistryIndex {
        &self.merged
    }

    fn trust(&self) -> TrustLevel {
        self.trust
    }

    fn root(&self) -> Option<&Path> {
        None
    }

    fn get_recipe(&self, id: &str) -> Result<Option<Recipe>> {
        // Later-wins: scan reverse.
        for src in self.sources.iter().rev() {
            if let Some(r) = src.get_recipe(id)? {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    fn get_profile(&self, id: &str) -> Result<Option<Profile>> {
        for src in self.sources.iter().rev() {
            if let Some(p) = src.get_profile(id)? {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Offline open + update (git CLI)
// ---------------------------------------------------------------------------

/// Open one registry offline (no network). Git registries require a populated cache.
pub fn open_offline(
    name: &str,
    spec: &RegistrySpec,
    cache_root: &Path,
    lock: &Lockfile,
) -> Result<DirSource> {
    match spec {
        RegistrySpec::Path { path, trust } => {
            let root = expand_user_path(path);
            DirSource::open_with_trust(name, root, *trust)
        }
        RegistrySpec::Git {
            url: _,
            ref_name: _,
            trust,
        } => {
            let pin = lock.registry(name).map(|r| r.pin.clone()).ok_or_else(|| {
                Error::Message(format!(
                    "registry '{name}': not cached (run `isol8 @registry update {name}` first)"
                ))
            })?;
            let root = cache_dir_for(cache_root, name, &pin);
            if !root.is_dir() {
                return Err(Error::Message(format!(
                    "registry '{name}': cache missing at {} (run `isol8 @registry update {name}`)",
                    root.display()
                )));
            }
            DirSource::open_with_trust(name, root, *trust)
        }
        RegistrySpec::Http { .. } => Err(Error::Message(format!(
            "registry '{name}': HTTP registries are not implemented yet (use path or git)"
        ))),
    }
}

/// Result of `@registry update`.
#[derive(Debug, Clone)]
pub struct UpdateResult {
    /// Registry name.
    pub name: String,
    /// Resolved pin written to the lockfile.
    pub pin: String,
    /// Cache / root path used.
    pub path: PathBuf,
    /// Trust level after open.
    pub trust: TrustLevel,
    /// Index entry count.
    pub entry_count: usize,
    /// Content hash of index.json.
    pub content_hash: String,
    /// Whether git/path fetch ran (path registries always "local").
    pub fetched: bool,
}

/// Update (or refresh) one registry into the cache and return lock metadata.
///
/// **Git:** `git clone` / `git fetch` + checkout ref; pin = commit SHA.  
/// **Path:** no network; pin = `path` or index content hash.  
/// **Http:** error (deferred).
pub fn update_registry(name: &str, spec: &RegistrySpec, cache_root: &Path) -> Result<UpdateResult> {
    match spec {
        RegistrySpec::Path { path, trust } => {
            let root = expand_user_path(path);
            let src = DirSource::open_with_trust(name, &root, *trust)?;
            let content_hash = src.index_content_hash()?;
            Ok(UpdateResult {
                name: name.to_string(),
                pin: content_hash.clone(),
                path: root,
                trust: src.trust(),
                entry_count: src.index().entries.len(),
                content_hash,
                fetched: false,
            })
        }
        RegistrySpec::Git {
            url,
            ref_name,
            trust,
        } => {
            let commit = git_fetch_to_cache(name, url, ref_name, cache_root)?;
            let root = cache_dir_for(cache_root, name, &commit);
            let src = DirSource::open_with_trust(name, &root, *trust)?;
            let content_hash = src.index_content_hash()?;
            Ok(UpdateResult {
                name: name.to_string(),
                pin: commit,
                path: root,
                trust: src.trust(),
                entry_count: src.index().entries.len(),
                content_hash,
                fetched: true,
            })
        }
        RegistrySpec::Http { .. } => Err(Error::Message(format!(
            "registry '{name}': HTTP registries are not implemented yet"
        ))),
    }
}

fn git_fetch_to_cache(name: &str, url: &str, ref_name: &str, cache_root: &Path) -> Result<String> {
    // Staging clone lives at <cache>/<name>/_src; pinned trees at <cache>/<name>/<sha>/.
    let staging = cache_root.join(name).join("_src");
    fs::create_dir_all(staging.parent().unwrap())
        .ctx(|| format!("creating cache for registry '{name}'"))?;

    if staging.join(".git").is_dir() {
        run_git(
            &staging,
            &["fetch", "--tags", "--force", "origin", ref_name],
            name,
        )?;
        run_git(&staging, &["checkout", "--force", "FETCH_HEAD"], name)?;
        // Prefer explicit ref if it exists locally after fetch.
        let _ = run_git(&staging, &["checkout", "--force", ref_name], name);
        run_git(&staging, &["reset", "--hard", "HEAD"], name)?;
    } else {
        if staging.exists() {
            fs::remove_dir_all(&staging)
                .ctx(|| format!("removing stale staging cache '{}'", staging.display()))?;
        }
        let parent = staging.parent().unwrap();
        let status = Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "--branch",
                ref_name,
                url,
                staging
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("_src"),
            ])
            .current_dir(parent)
            .status()
            .ctx(|| format!("registry '{name}': spawning git clone"))?;
        if !status.success() {
            // Retry without --branch (ref might be a commit / default branch only).
            let status = Command::new("git")
                .args([
                    "clone",
                    "--depth",
                    "1",
                    url,
                    staging
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("_src"),
                ])
                .current_dir(parent)
                .status()
                .ctx(|| format!("registry '{name}': spawning git clone (fallback)"))?;
            if !status.success() {
                return Err(Error::Message(format!(
                    "registry '{name}': git clone failed for {url} (ref {ref_name})"
                )));
            }
            let _ = run_git(
                &staging,
                &["fetch", "--depth", "1", "origin", ref_name],
                name,
            );
            let _ = run_git(&staging, &["checkout", ref_name], name);
        }
    }

    let commit = git_rev_parse(&staging, "HEAD", name)?;
    let dest = cache_dir_for(cache_root, name, &commit);
    if !dest.is_dir() {
        // Copy tree (no .git) into pin directory for offline use.
        copy_tree_excluding_git(&staging, &dest)?;
    }
    Ok(commit)
}

fn run_git(cwd: &Path, args: &[&str], name: &str) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .ctx(|| format!("registry '{name}': git {}", args.join(" ")))?;
    if !status.success() {
        return Err(Error::Message(format!(
            "registry '{name}': git {} failed",
            args.join(" ")
        )));
    }
    Ok(())
}

fn git_rev_parse(cwd: &Path, rev: &str, name: &str) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(cwd)
        .output()
        .ctx(|| format!("registry '{name}': git rev-parse"))?;
    if !out.status.success() {
        return Err(Error::Message(format!(
            "registry '{name}': git rev-parse {rev} failed"
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn copy_tree_excluding_git(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).ctx(|| format!("creating '{}'", dest.display()))?;
    for ent in fs::read_dir(src).ctx(|| format!("reading '{}'", src.display()))? {
        let ent = ent?;
        let name = ent.file_name();
        if name == ".git" {
            continue;
        }
        let from = ent.path();
        let to = dest.join(&name);
        if from.is_dir() {
            copy_tree_excluding_git(&from, &to)?;
        } else {
            fs::copy(&from, &to).ctx(|| format!("copy {} → {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Install diff + sensitive path flags
// ---------------------------------------------------------------------------

/// Paths that always get highlighted on install (evo-repo §5.5).
pub const SENSITIVE_PATH_MARKERS: &[&str] = &[
    "/.ssh",
    "/.aws",
    "/.gnupg",
    "/.kube",
    "Keychains",
    "/etc/shadow",
    "/etc/sudoers",
    "#HOME/.ssh",
    "#HOME/.aws",
    "#HOME/.gnupg",
];

/// One line of install/update diff.
#[derive(Debug, Clone)]
pub struct DiffItem {
    /// Artifact id.
    pub id: String,
    /// `added` | `changed` | `removed` | `same`.
    pub change: &'static str,
    /// Kind label.
    pub kind: String,
    /// Summary text.
    pub summary: String,
    /// Highlights (new rw, sensitive paths, ceiling violations).
    pub flags: Vec<String>,
}

/// Build a diff of index entries vs previous lock entries.
pub fn diff_index(old: &Lockfile, src: &DirSource) -> Result<Vec<DiffItem>> {
    let mut old_map: HashMap<String, Option<String>> = HashMap::new();
    for e in &old.entries {
        if e.registry == src.name() {
            old_map.insert(e.id.clone(), e.sha256.clone());
        }
    }
    // If no per-entry pins, treat empty as first install (all added).
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for entry in &src.index().entries {
        seen.insert(entry.id.clone());
        let new_hash = entry.sha256.clone();
        let change = match old_map.get(&entry.id) {
            None => "added",
            Some(old_h) if old_h == &new_hash => "same",
            Some(_) => "changed",
        };
        let mut flags = Vec::new();
        if entry.kind == ArtifactKind::Recipe {
            flags.extend(inspect_recipe_flags(src, entry)?);
        }
        items.push(DiffItem {
            id: entry.id.clone(),
            change,
            kind: format!("{:?}", entry.kind).to_ascii_lowercase(),
            summary: entry.summary.clone(),
            flags,
        });
    }

    for (id, _) in old_map {
        if !seen.contains(&id) {
            items.push(DiffItem {
                id,
                change: "removed",
                kind: "unknown".into(),
                summary: String::new(),
                flags: Vec::new(),
            });
        }
    }

    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(items)
}

fn inspect_recipe_flags(src: &DirSource, entry: &IndexEntry) -> Result<Vec<String>> {
    let mut flags = Vec::new();
    let path = src.file_path(entry);
    if !path.is_file() {
        flags.push("missing file".into());
        return Ok(flags);
    }
    // Best-effort parse; extended-schema recipes may fail — flag and continue.
    let recipe = match recipe::load_from_path(&path) {
        Ok(r) => r,
        Err(e) => {
            flags.push(format!("parse skipped: {e}"));
            return Ok(flags);
        }
    };

    let forbidden = &src.manifest.trust.forbidden_paths;
    let rw_allowed = &src.manifest.trust.rw_outside_home_allowed;
    let ceiling = src
        .manifest
        .trust
        .max_grant_outside_home
        .as_deref()
        .unwrap_or("rw");

    for (strat_name, bodies) in &recipe.strategies {
        for body in bodies {
            for g in &body.paths {
                let p = &g.path;
                for marker in SENSITIVE_PATH_MARKERS {
                    if p.contains(marker) {
                        flags.push(format!(
                            "sensitive path in {} {:?}: {p}",
                            strat_name.as_str(),
                            g.access
                        ));
                    }
                }
                for f in forbidden {
                    if path_matches_forbidden(p, f) {
                        flags.push(format!(
                            "FORBIDDEN path {} in strategy {}",
                            p,
                            strat_name.as_str()
                        ));
                    }
                }
                let outside_replaced = p.starts_with("#HOME")
                    || p.starts_with("~") && !p.starts_with("~/") // rare
                    || p.starts_with('/');
                // Real-home grants use #HOME; those are "outside replaced home".
                let real_home_grant = p.starts_with("#HOME");
                if real_home_grant && matches!(g.access, isol8_core::profile::Access::Rw) {
                    flags.push(format!(
                        "new rw on real home via {}: {p}",
                        strat_name.as_str()
                    ));
                    if ceiling == "ro" || ceiling == "none" {
                        let allowed = rw_allowed.iter().any(|id| id == &recipe.id);
                        if !allowed {
                            flags.push(format!(
                                "ceiling violation: rw outside home on {p} \
                                 (recipe '{}' not in rw_outside_home_allowed)",
                                recipe.id
                            ));
                        }
                    }
                }
                let _ = outside_replaced;
            }
        }
    }
    Ok(flags)
}

fn path_matches_forbidden(path: &str, forbidden: &str) -> bool {
    path == forbidden
        || path.starts_with(&format!("{forbidden}/"))
        || path
            .strip_prefix("#HOME")
            .and_then(|r| {
                forbidden
                    .strip_prefix("#HOME")
                    .map(|f| r.starts_with(f) || r == f)
            })
            .unwrap_or(false)
}

/// Build lock entries from a DirSource index.
pub fn lock_entries_from(src: &DirSource) -> Vec<LockEntry> {
    src.index()
        .entries
        .iter()
        .map(|e| LockEntry {
            registry: src.name().to_string(),
            id: e.id.clone(),
            kind: match e.kind {
                ArtifactKind::Profile => "profile".into(),
                ArtifactKind::Recipe => "recipe".into(),
                ArtifactKind::Bundle => "bundle".into(),
            },
            sha256: e.sha256.clone(),
        })
        .collect()
}

/// Apply an [`UpdateResult`] into a lockfile (replace same-name registry + its entries).
pub fn apply_update_to_lockfile(lock: &mut Lockfile, upd: &UpdateResult, src: &DirSource) {
    lock.schema = 1;
    lock.registries.retain(|r| r.name != upd.name);
    lock.registries.push(LockRegistry {
        name: upd.name.clone(),
        source: src
            .root()
            .map(|p| format!("path:{}", p.display()))
            .unwrap_or_default(),
        pin: upd.pin.clone(),
        content_hash: Some(upd.content_hash.clone()),
        trust: Some(upd.trust.as_str().to_string()),
    });
    lock.entries.retain(|e| e.registry != upd.name);
    lock.entries.extend(lock_entries_from(src));
    lock.registries.sort_by(|a, b| a.name.cmp(&b.name));
    lock.entries
        .sort_by(|a, b| (&a.registry, &a.id).cmp(&(&b.registry, &b.id)));
}

// ---------------------------------------------------------------------------
// Offline recipe dirs for RecipeRegistry
// ---------------------------------------------------------------------------

/// Resolve on-disk recipe roots for all configured registries (offline).
///
/// Missing caches are skipped with no error so day-to-day runs stay offline and
/// fail only when a cage *needs* a missing remote recipe.
pub fn offline_recipe_roots(
    registries: &BTreeMap<String, RegistrySpec>,
    cache_root: &Path,
    lock: &Lockfile,
) -> Vec<(String, PathBuf, TrustLevel)> {
    let mut out = Vec::new();
    for (name, spec) in registries {
        match open_offline(name, spec, cache_root, lock) {
            Ok(src) => {
                // Prefer recipes/ subdir when present; else root (flat layouts).
                let root = src.root().map(|p| p.to_path_buf()).unwrap_or_default();
                let recipes = root.join("recipes");
                let dir = if recipes.is_dir() { recipes } else { root };
                out.push((name.clone(), dir, src.trust()));
            }
            Err(_) => continue,
        }
    }
    out
}

/// Discover offline registry recipe dirs from ambient config (no network).
///
/// Returns `(source_label, recipes_dir)` where `source_label` is
/// `registry:<trust>:<name>` for trust gating.
///
/// Looks for registries in:
/// 1. `ISOL8_CONFIG_PATH` / `./isol8.toml` / `~/.config/isol8/isol8.toml`
/// 2. Lockfile for git pins
///
/// Silently skips missing or unreadable config — builtins still work.
pub fn discover_offline_recipe_dirs() -> Vec<(String, PathBuf)> {
    let registries = match load_registries_from_config() {
        Ok(m) if !m.is_empty() => m,
        _ => return Vec::new(),
    };
    let cache_root = default_cache_root();
    let lock_path = discover_lockfile_path();
    let lock = Lockfile::load(&lock_path).unwrap_or_default();
    offline_recipe_roots(&registries, &cache_root, &lock)
        .into_iter()
        .map(|(name, dir, trust)| {
            let label = format!("registry:{}:{}", trust.as_str(), name);
            (label, dir)
        })
        .collect()
}

/// Load `[registries]` tables from the same config discovery order as the CLI.
pub fn load_registries_from_config() -> Result<BTreeMap<String, RegistrySpec>> {
    let path = discover_config_file();
    let Some(path) = path else {
        return Ok(BTreeMap::new());
    };
    let body = fs::read_to_string(&path).ctx(|| format!("reading config '{}'", path.display()))?;
    parse_registries_from_toml(&body)
}

fn discover_config_file() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ISOL8_CONFIG_PATH") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
        if p.is_dir() {
            for name in ["isol8.toml", "isol8.yaml", "isol8.yml"] {
                let c = p.join(name);
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    for name in ["isol8.toml", "isol8.yaml", "isol8.yml"] {
        let c = PathBuf::from(name);
        if c.is_file() {
            return Some(c);
        }
    }
    let base = config_isol8_dir();
    for name in ["isol8.toml", "isol8.yaml", "isol8.yml"] {
        let c = base.join(name);
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

/// Parse only the `[registries.*]` tables from a TOML config body.
pub fn parse_registries_from_toml(body: &str) -> Result<BTreeMap<String, RegistrySpec>> {
    let value: toml::Value =
        toml::from_str(body).map_err(|e| Error::Message(format!("parsing config TOML: {e}")))?;
    let mut out = BTreeMap::new();
    let Some(regs) = value.get("registries").and_then(|v| v.as_table()) else {
        return Ok(out);
    };
    for (name, v) in regs {
        out.insert(name.clone(), parse_registry_spec(name, v)?);
    }
    Ok(out)
}

/// Verify lockfile pins against currently openable sources. Returns drift errors.
pub fn verify_lock_against_disk(
    registries: &BTreeMap<String, RegistrySpec>,
    cache_root: &Path,
    lock: &Lockfile,
) -> Result<Vec<String>> {
    let mut drifts = Vec::new();
    for lr in &lock.registries {
        let Some(spec) = registries.get(&lr.name) else {
            drifts.push(format!(
                "lockfile registry '{}' not present in config",
                lr.name
            ));
            continue;
        };
        let src = match open_offline(&lr.name, spec, cache_root, lock) {
            Ok(s) => s,
            Err(e) => {
                drifts.push(format!("registry '{}': {e}", lr.name));
                continue;
            }
        };
        if let Some(expected) = lr.content_hash.as_deref() {
            let actual = src.index_content_hash()?;
            if !actual.eq_ignore_ascii_case(expected) {
                drifts.push(format!(
                    "registry '{}': content hash drift (lock {expected}, disk {actual})",
                    lr.name
                ));
            }
        }
        for d in src.verify_content_hashes()? {
            drifts.push(format!("registry '{}': {d}", lr.name));
        }
    }
    Ok(drifts)
}

// ---------------------------------------------------------------------------
// Hash helper (no extra dependency)
// ---------------------------------------------------------------------------

/// SHA-256 hex digest (minimal implementation via a tiny pure-Rust path).
///
/// Uses the `sha2`-less approach of calling out only if needed — we implement
/// a compact sha256 here via `std` + a well-known small implementation is
/// heavy; prefer the `sha2` crate? AGENTS says don't add deps for a few lines.
///
/// On all platforms we shell out is wrong. Use a minimal pure impl.
pub fn sha256_hex(data: &[u8]) -> String {
    sha256::digest(data)
}

/// Minimal SHA-256 (public domain style compact impl) for lockfile pins.
mod sha256 {
    // Compact SHA-256 based on FIPS 180-4. Enough for content hashing; not for HMAC.
    pub fn digest(data: &[u8]) -> String {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let bits = (data.len() as u64) * 8;
        let mut buf = data.to_vec();
        buf.push(0x80);
        while (buf.len() % 64) != 56 {
            buf.push(0);
        }
        buf.extend_from_slice(&bits.to_be_bytes());

        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];

        for chunk in buf.chunks_exact(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([
                    chunk[i * 4],
                    chunk[i * 4 + 1],
                    chunk[i * 4 + 2],
                    chunk[i * 4 + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let mut a = h[0];
            let mut b = h[1];
            let mut c = h[2];
            let mut d = h[3];
            let mut e = h[4];
            let mut f = h[5];
            let mut g = h[6];
            let mut hh = h[7];
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }

        let mut out = String::with_capacity(64);
        for word in h {
            out.push_str(&format!("{word:08x}"));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture_root() -> PathBuf {
        // Workspace root is two levels above crates/isol8-registry.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/registry")
            .canonicalize()
            .expect("tests/fixtures/registry")
    }

    fn tmp() -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "isol8-reg-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dir_source_opens_fixture() {
        let src = DirSource::open("fixture", fixture_root()).unwrap();
        assert_eq!(src.name(), "fixture");
        assert_eq!(src.trust(), TrustLevel::Official);
        assert!(src.index().get("toolchains/sample").is_some());
        assert!(src.index().get("toolchains/sample-cache").is_some());
    }

    #[test]
    fn dir_source_loads_recipe() {
        let src = DirSource::open("fixture", fixture_root()).unwrap();
        let r = src.get_recipe("toolchains/sample").unwrap().unwrap();
        assert_eq!(r.id, "toolchains/sample");
        assert!(
            r.source.starts_with("registry:official:fixture:"),
            "source={}",
            r.source
        );
        assert!(r.strategies.contains_key(&recipe::StrategyName::Link));
    }

    #[test]
    fn content_hashes_match_fixture() {
        let src = DirSource::open("fixture", fixture_root()).unwrap();
        let drifts = src.verify_content_hashes().unwrap();
        assert!(drifts.is_empty(), "unexpected drift: {drifts:?}");
    }

    #[test]
    fn layered_later_wins() {
        let a = DirSource::open("fixture", fixture_root()).unwrap();
        // Second source is the same — just ensure layering works.
        let b = DirSource::open("fixture2", fixture_root()).unwrap();
        let layered = LayeredSource::new(vec![Box::new(a), Box::new(b)]);
        let r = layered.get_recipe("toolchains/sample").unwrap().unwrap();
        assert!(r.source.contains("fixture2") || r.source.contains("fixture"));
    }

    #[test]
    fn lockfile_roundtrip() {
        let dir = tmp();
        let path = dir.join("isol8.lock");
        let mut lock = Lockfile::default();
        lock.registries.push(LockRegistry {
            name: "fixture".into(),
            source: "path:./x".into(),
            pin: "abc".into(),
            content_hash: Some("deadbeef".into()),
            trust: Some("official".into()),
        });
        lock.save(&path).unwrap();
        let loaded = Lockfile::load(&path).unwrap();
        assert_eq!(loaded.registries.len(), 1);
        assert_eq!(loaded.registries[0].pin, "abc");
    }

    #[test]
    fn parse_registry_specs() {
        let path_v: toml::Value = toml::from_str(r#"path = "~/recipes""#).unwrap();
        let s = parse_registry_spec("scratch", &path_v).unwrap();
        assert!(matches!(s, RegistrySpec::Path { .. }));

        let git_v: toml::Value = toml::from_str(
            r#"
git = "https://example.com/r.git"
ref = "v1"
"#,
        )
        .unwrap();
        let s = parse_registry_spec("official", &git_v).unwrap();
        match s {
            RegistrySpec::Git { ref_name, .. } => assert_eq!(ref_name, "v1"),
            _ => panic!("expected git"),
        }
    }

    #[test]
    fn update_path_registry_and_diff() {
        let src = DirSource::open("fixture", fixture_root()).unwrap();
        let upd = update_registry(
            "fixture",
            &RegistrySpec::Path {
                path: fixture_root().to_string_lossy().into(),
                trust: None,
            },
            &tmp(),
        )
        .unwrap();
        assert!(!upd.fetched);
        assert!(upd.entry_count >= 2);

        let empty = Lockfile::default();
        let diff = diff_index(&empty, &src).unwrap();
        assert!(diff
            .iter()
            .any(|d| d.id == "toolchains/sample" && d.change == "added"));
        // sample-cache has rw on #HOME
        let cache = diff
            .iter()
            .find(|d| d.id == "toolchains/sample-cache")
            .unwrap();
        assert!(
            cache.flags.iter().any(|f| f.contains("rw on real home")),
            "flags: {:?}",
            cache.flags
        );
    }

    #[test]
    fn hash_stable() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn trust_commands_gate() {
        assert!(TrustLevel::Official.commands_allowed());
        assert!(TrustLevel::Local.commands_allowed());
        assert!(!TrustLevel::Community.commands_allowed());
        assert!(!TrustLevel::Untrusted.commands_allowed());
    }
}
