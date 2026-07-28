//! User configuration: discovery, parsing, merge, and `ISOL8_*` overrides.
//!
//! This is the **single** implementation of the rules documented in
//! [`_docs/config.md`](../../_docs/config.md). Both the CLI and the registry crate
//! consume it; neither reimplements discovery.
//!
//! # Discovery order
//!
//! 1. `ISOL8_CONFIG_PATH` (file, or directory containing `isol8.toml` / yaml) —
//!    absolute override; no local merge.
//! 2. Project-local marker in cwd (first match wins):
//!    `isol8.toml`, `.isol8.toml`, `encage.toml`, `.encage.toml`
//!    - `config_path = "…"` redirects the base config (same as the env var)
//!    - `ignore_global = true` skips OS / redirected base entirely
//!    - other fields merge onto the base (local wins)
//! 3. OS default: `$XDG_CONFIG_HOME/isol8/` or `~/.config/isol8/`
//!    (Windows: `%APPDATA%/isol8/`)
//!
//! # Path tokens
//!
//! Paths starting with `@` resolve relative to the **effective config directory**
//! (see [`effective_config_dir`]). Everything else is absolutized against the
//! process cwd so a later `chdir` cannot retarget it.
//!
//! # Ambient vs hermetic
//!
//! [`load()`](crate::config::load) reads the process environment and cwd — the
//! CLI entry point. [`load_in()`](crate::config::load_in) takes an explicit
//! [`Context`] and reads **no** environment variables, which is what an
//! in-process host (or a hermetic test) wants.
//!
//! ```no_run
//! let cfg = isol8_core::config::load()?;
//! assert!(!cfg.default_profiles.is_empty());
//! # Ok::<(), isol8_core::Error>(())
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::context::{self, Context};
use crate::error::{Error, Result, ResultExt};

/// Project-local config markers (cwd). First existing file wins.
pub const PROJECT_CONFIG_MARKERS: &[&str] =
    &["isol8.toml", ".isol8.toml", "encage.toml", ".encage.toml"];

/// Canonical config basenames inside a config directory.
pub const CONFIG_BASENAMES: &[&str] = &["isol8.toml", "isol8.yaml", "isol8.yml"];

/// User-facing config (`isol8.toml` / `isol8.yaml`).
///
/// `registries` stays as raw TOML tables so this crate needs no dependency on
/// `isol8-registry`; that crate turns them into typed `RegistrySpec`s.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Profile layers always selected (deny-first merge order).
    pub default_profiles: Vec<String>,
    /// Select layers whose `filter.executables` match the command.
    pub auto_profiles: bool,
    /// Extra profile files or directories (later wins on name collision).
    pub profile_paths: Vec<String>,
    /// Extra read-write path grants.
    pub add_dirs_rw: Vec<String>,
    /// Extra read-only path grants.
    pub add_dirs_ro: Vec<String>,
    /// Replacement `$HOME` for the confined process.
    pub home: Option<String>,
    /// Named cage to use when `--cage` / `ISOL8_CAGE` are unset.
    pub cage: Option<String>,
    /// Print the effective policy and exit instead of spawning.
    pub dry_run: bool,
    /// Raw `[registries.<name>]` tables, `@`-expanded. Typed by `isol8-registry`.
    #[serde(default, skip_deserializing)]
    pub registries: BTreeMap<String, toml::Value>,
}

impl Config {
    /// OS-specific defaults used by `isol8 @init` and when no config file exists.
    pub fn builtin_defaults() -> Self {
        let system = if cfg!(target_os = "macos") {
            "macos/system-runtime"
        } else if cfg!(target_os = "linux") {
            "linux/system-runtime"
        } else if cfg!(target_os = "windows") {
            "windows/system-runtime"
        } else {
            "base"
        };
        Self {
            default_profiles: vec!["base".into(), system.into()],
            auto_profiles: true,
            ..Default::default()
        }
    }
}

/// Partial / raw file shape: `Option` fields distinguish absent vs set for merge.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawConfig {
    /// Redirect the base config (file or directory). Like `ISOL8_CONFIG_PATH`.
    config_path: Option<String>,
    /// When true, do not load or merge the OS / redirected base config.
    ignore_global: bool,
    default_profiles: Option<Vec<String>>,
    auto_profiles: Option<bool>,
    profile_paths: Option<Vec<String>>,
    add_dirs_rw: Option<Vec<String>>,
    add_dirs_ro: Option<Vec<String>>,
    home: Option<String>,
    cage: Option<String>,
    dry_run: Option<bool>,
    #[serde(default, skip_deserializing)]
    registries: BTreeMap<String, toml::Value>,
}

/// Lightweight peek of `config_path` / `ignore_global` from a project marker.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LocalMeta {
    config_path: Option<String>,
    ignore_global: bool,
}

// ---------------------------------------------------------------------------
// Discovery primitives
// ---------------------------------------------------------------------------

/// OS config directory for isol8 (`~/.config/isol8` on macOS/Linux).
pub fn os_config_dir() -> PathBuf {
    context::default_os_config_dir(&context::real_home_from_env())
}

/// First project-local marker in `dir`.
pub fn discover_local_marker_in(dir: &Path) -> Option<PathBuf> {
    for name in PROJECT_CONFIG_MARKERS {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// First project-local marker in the process cwd (relative paths as returned).
pub fn discover_local_marker() -> Option<PathBuf> {
    for name in PROJECT_CONFIG_MARKERS {
        let candidate = PathBuf::from(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Resolve a config path value (file, or directory containing a config basename).
pub fn resolve_config_location(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if path.is_dir() {
        return find_config_in_dir(path);
    }
    None
}

/// First existing config basename inside `dir`.
pub fn find_config_in_dir(dir: &Path) -> Option<PathBuf> {
    for name in CONFIG_BASENAMES {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Expand `@…` relative to `config_dir`; absolutize everything else.
///
/// Non-`@` relative paths join the **process cwd**, not the config dir, so the
/// result survives a later `chdir`.
pub fn expand_at_path(path: &str, config_dir: &Path) -> String {
    if let Some(p) = context::expand_at_path(path, config_dir) {
        return p.display().to_string();
    }
    context::absolute_path(Path::new(path))
        .display()
        .to_string()
}

fn peek_local_meta(path: &Path) -> Option<LocalMeta> {
    let body = std::fs::read_to_string(path).ok()?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("toml");
    match ext {
        "yaml" | "yml" => parse_yaml_meta(&body),
        _ => toml::from_str(&body).ok(),
    }
}

#[cfg(feature = "yaml")]
fn parse_yaml_meta(body: &str) -> Option<LocalMeta> {
    serde_yaml::from_str(body).ok()
}

#[cfg(not(feature = "yaml"))]
fn parse_yaml_meta(_body: &str) -> Option<LocalMeta> {
    None
}

/// Map a config location (file or directory, may not exist yet) to its root dir.
fn config_root_from_location(path: &Path) -> PathBuf {
    if let Some(file) = resolve_config_location(path) {
        return file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
    }
    if path.is_file() {
        return path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
    }
    if path.as_os_str().is_empty() {
        return PathBuf::from(".");
    }
    // Directory (existing or intended) used as a config root.
    path.to_path_buf()
}

/// Root directory of the effective isol8 config tree (`…/isol8` or a redirect).
///
/// 1. `ISOL8_CONFIG_PATH` (file → parent dir; directory → as-is)
/// 2. Project marker (`config_path` redirect, or marker parent if `ignore_global`)
/// 3. OS default (`~/.config/isol8`, `$XDG_CONFIG_HOME/isol8`, …)
///
/// Always **absolute**. Cages live at `{dir}/cages/`, wizard state at
/// `{dir}/state.toml`, `@managed/<id>` at `{dir}/homes/<id>`.
pub fn effective_config_dir() -> PathBuf {
    let raw = if let Ok(path) = std::env::var("ISOL8_CONFIG_PATH") {
        config_root_from_location(Path::new(&path))
    } else if let Some(local) = discover_local_marker() {
        match peek_local_meta(&local) {
            Some(meta) if meta.ignore_global => local
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
            Some(meta) => match meta.config_path.as_deref().filter(|s| !s.is_empty()) {
                Some(cp) => config_root_from_location(Path::new(cp)),
                None => os_config_dir(),
            },
            None => os_config_dir(),
        }
    } else {
        os_config_dir()
    };
    context::absolute_path(&raw)
}

/// Config-level cages directory: `{effective_config_dir()}/cages` (absolute).
pub fn effective_cages_dir() -> PathBuf {
    effective_config_dir().join("cages")
}

/// Resolved primary config file path (best-effort; for diagnostics).
pub fn discover_config_file() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ISOL8_CONFIG_PATH") {
        return resolve_config_location(Path::new(&path));
    }
    if let Some(local) = discover_local_marker() {
        if let Some(meta) = peek_local_meta(&local) {
            if meta.ignore_global {
                return Some(local);
            }
            if let Some(cp) = meta.config_path.as_deref().filter(|s| !s.is_empty()) {
                return resolve_config_location(Path::new(cp)).or(Some(local));
            }
        }
        if let Some(base) = find_config_in_dir(&os_config_dir()) {
            return Some(base);
        }
        return Some(local);
    }
    find_config_in_dir(&os_config_dir())
}

// ---------------------------------------------------------------------------
// Load
// ---------------------------------------------------------------------------

/// Load config using ambient discovery (env → local marker → OS default).
pub fn load() -> Result<Config> {
    // 1. Explicit env override — no local merge.
    if let Ok(path) = std::env::var("ISOL8_CONFIG_PATH") {
        let p = PathBuf::from(&path);
        let Some(file) = resolve_config_location(&p) else {
            return Err(Error::Message(format!(
                "ISOL8_CONFIG_PATH='{path}' is not a config file or a directory containing isol8.toml/yaml"
            )));
        };
        let mut cfg = load_file_as_base(&file)?;
        expand_config_paths(&mut cfg, &parent_or_dot(&file));
        return Ok(cfg);
    }
    load_with_marker(discover_local_marker(), &os_config_dir())
}

/// Load config from an explicit [`Context`] — reads **no** environment variables.
///
/// Marker discovery happens under [`Context::cwd`]; the base config is taken from
/// [`Context::config_dir`]. Use this from an in-process host whose environment
/// belongs to the host rather than to isol8.
pub fn load_in(ctx: &Context) -> Result<Config> {
    load_with_marker(discover_local_marker_in(&ctx.cwd), &ctx.config_dir)
}

fn load_with_marker(local: Option<PathBuf>, os_dir: &Path) -> Result<Config> {
    let local_raw = match &local {
        Some(path) => Some(load_raw(path)?),
        None => None,
    };

    let (base, config_dir) = match &local_raw {
        Some(raw) if raw.ignore_global => {
            let dir = local
                .as_ref()
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .filter(|d| !d.as_os_str().is_empty())
                .unwrap_or_else(|| PathBuf::from("."));
            (Config::builtin_defaults(), dir)
        }
        Some(raw) if raw.config_path.as_deref().is_some_and(|s| !s.is_empty()) => {
            let cp = raw.config_path.as_deref().unwrap_or_default();
            match resolve_config_location(Path::new(cp)) {
                Some(file) => (load_file_as_base(&file)?, parent_or_dot(&file)),
                None => {
                    return Err(Error::Message(format!(
                        "config_path='{cp}' in {} is not a config file or directory containing isol8.toml/yaml",
                        local.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
                    )))
                }
            }
        }
        _ => match find_config_in_dir(os_dir) {
            Some(file) => (load_file_as_base(&file)?, parent_or_dot(&file)),
            None => (Config::builtin_defaults(), os_dir.to_path_buf()),
        },
    };

    let mut cfg = match local_raw {
        Some(raw) => merge_overlay(base, raw),
        None => base,
    };
    expand_config_paths(&mut cfg, &config_dir);
    Ok(cfg)
}

fn parent_or_dot(file: &Path) -> PathBuf {
    file.parent()
        .map(Path::to_path_buf)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn load_raw(path: &Path) -> Result<RawConfig> {
    let body =
        std::fs::read_to_string(path).ctx(|| format!("reading config '{}'", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("toml");
    let (stripped, registries) = strip_registries(&body, ext, path)?;
    let mut raw: RawConfig = match ext {
        "yaml" | "yml" => parse_yaml_config(&stripped, path)?,
        _ => {
            toml::from_str(&stripped).ctx(|| format!("parsing TOML config '{}'", path.display()))?
        }
    };
    raw.registries = registries;
    Ok(raw)
}

#[cfg(feature = "yaml")]
fn parse_yaml_config(body: &str, path: &Path) -> Result<RawConfig> {
    serde_yaml::from_str(body)
        .map_err(|e| Error::Message(format!("parsing YAML config '{}': {e}", path.display())))
}

#[cfg(not(feature = "yaml"))]
fn parse_yaml_config(_body: &str, path: &Path) -> Result<RawConfig> {
    Err(Error::Message(format!(
        "YAML config '{}' requires the `yaml` feature of isol8-core",
        path.display()
    )))
}

fn load_file_as_base(path: &Path) -> Result<Config> {
    Ok(raw_to_base(load_raw(path)?))
}

/// Convert a full config file into a `Config` (defaults fill absent fields).
fn raw_to_base(raw: RawConfig) -> Config {
    let defaults = Config::builtin_defaults();
    let default_profiles = match raw.default_profiles {
        Some(v) if !v.is_empty() => v,
        _ => defaults.default_profiles,
    };
    Config {
        default_profiles,
        auto_profiles: raw.auto_profiles.unwrap_or(defaults.auto_profiles),
        profile_paths: raw.profile_paths.unwrap_or_default(),
        add_dirs_rw: raw.add_dirs_rw.unwrap_or_default(),
        add_dirs_ro: raw.add_dirs_ro.unwrap_or_default(),
        home: raw.home,
        cage: raw.cage,
        dry_run: raw.dry_run.unwrap_or(false),
        registries: raw.registries,
    }
}

/// Merge local overlay onto base: only fields present in the overlay replace base.
fn merge_overlay(mut base: Config, overlay: RawConfig) -> Config {
    if let Some(v) = overlay.default_profiles {
        if !v.is_empty() {
            base.default_profiles = v;
        }
    }
    if let Some(v) = overlay.auto_profiles {
        base.auto_profiles = v;
    }
    if let Some(v) = overlay.profile_paths {
        base.profile_paths = v;
    }
    if let Some(v) = overlay.add_dirs_rw {
        base.add_dirs_rw = v;
    }
    if let Some(v) = overlay.add_dirs_ro {
        base.add_dirs_ro = v;
    }
    if overlay.home.is_some() {
        base.home = overlay.home;
    }
    if overlay.cage.is_some() {
        base.cage = overlay.cage;
    }
    if let Some(v) = overlay.dry_run {
        base.dry_run = v;
    }
    // Local registry names override; others from base are kept.
    for (k, v) in overlay.registries {
        base.registries.insert(k, v);
    }
    base
}

/// Remove `[registries]` from the body (so `deny_unknown_fields` succeeds) and
/// return the stripped body plus the raw registry tables.
fn strip_registries(
    body: &str,
    ext: &str,
    path: &Path,
) -> Result<(String, BTreeMap<String, toml::Value>)> {
    if matches!(ext, "yaml" | "yml") {
        return strip_registries_yaml(body, path);
    }
    let value: toml::Value = toml::from_str(body)
        .ctx(|| format!("parsing TOML config '{}' for registries", path.display()))?;
    let mut table = value
        .as_table()
        .cloned()
        .ok_or_else(|| Error::Message("config root must be a table".into()))?;
    let regs = match table.remove("registries") {
        Some(toml::Value::Table(t)) => t.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    let stripped = toml::to_string(&toml::Value::Table(table))
        .map_err(|e| Error::Message(format!("re-serializing config without registries: {e}")))?;
    Ok((stripped, regs))
}

#[cfg(feature = "yaml")]
fn strip_registries_yaml(
    body: &str,
    path: &Path,
) -> Result<(String, BTreeMap<String, toml::Value>)> {
    let mut value: serde_yaml::Value = serde_yaml::from_str(body)
        .map_err(|e| Error::Message(format!("parsing YAML '{}': {e}", path.display())))?;
    let mut regs = BTreeMap::new();
    if let Some(map) = value.as_mapping_mut() {
        let key = serde_yaml::Value::String("registries".into());
        if let Some(reg_val) = map.remove(&key) {
            if let Some(reg_map) = reg_val.as_mapping() {
                for (k, v) in reg_map {
                    let name = k.as_str().unwrap_or("").to_string();
                    if name.is_empty() {
                        continue;
                    }
                    // YAML → JSON → TOML so registry parsing stays TOML-shaped.
                    let json = serde_json::to_value(v).map_err(|e| {
                        Error::Message(format!("registries.{name}: converting YAML entry: {e}"))
                    })?;
                    let toml_v: toml::Value = serde_json::from_value(json).map_err(|e| {
                        Error::Message(format!("registries.{name}: converting to TOML: {e}"))
                    })?;
                    regs.insert(name, toml_v);
                }
            }
        }
    }
    let stripped = serde_yaml::to_string(&value)
        .map_err(|e| Error::Message(format!("re-serializing YAML without registries: {e}")))?;
    Ok((stripped, regs))
}

#[cfg(not(feature = "yaml"))]
fn strip_registries_yaml(
    _body: &str,
    path: &Path,
) -> Result<(String, BTreeMap<String, toml::Value>)> {
    Err(Error::Message(format!(
        "YAML config '{}' requires the `yaml` feature of isol8-core",
        path.display()
    )))
}

fn expand_at_paths(paths: &mut [String], config_dir: &Path) {
    for p in paths.iter_mut() {
        *p = expand_at_path(p, config_dir);
    }
}

fn expand_config_paths(cfg: &mut Config, config_dir: &Path) {
    let config_dir = context::absolute_path(config_dir);
    expand_at_paths(&mut cfg.profile_paths, &config_dir);
    expand_at_paths(&mut cfg.add_dirs_rw, &config_dir);
    expand_at_paths(&mut cfg.add_dirs_ro, &config_dir);
    if let Some(ref home) = cfg.home {
        cfg.home = Some(expand_at_path(home, &config_dir));
    }
    // Registry `path = "@…"` entries expand against the same root.
    for spec in cfg.registries.values_mut() {
        let Some(table) = spec.as_table_mut() else {
            continue;
        };
        let Some(raw) = table.get("path").and_then(|v| v.as_str()) else {
            continue;
        };
        let expanded = expand_at_path(raw, &config_dir);
        table.insert("path".into(), toml::Value::String(expanded));
    }
}

// ---------------------------------------------------------------------------
// Environment overrides
// ---------------------------------------------------------------------------

/// Apply `ISOL8_*` overrides onto a loaded [`Config`].
///
/// Sits between the config file and CLI flags in precedence
/// ([`_docs/config.md`](../../_docs/config.md) §7). `ISOL8_AUTO_PROFILES` is
/// applied here; a CLI `--auto-profiles` / `--no-auto-profiles` wins afterwards.
pub fn apply_env_overrides(cfg: &mut Config) {
    if let Some(v) = non_empty_env("ISOL8_PROFILE") {
        cfg.default_profiles = split_list(&v);
    }
    if let Some(v) = non_empty_env("ISOL8_PROFILE_PATH") {
        cfg.profile_paths = split_list(&v);
    }
    if let Some(v) = non_empty_env("ISOL8_ADD_DIRS_RW") {
        cfg.add_dirs_rw = split_list(&v);
    }
    if let Some(v) = non_empty_env("ISOL8_ADD_DIRS_RO") {
        cfg.add_dirs_ro = split_list(&v);
    }
    if let Some(v) = non_empty_env("ISOL8_HOME") {
        cfg.home = Some(v);
    }
    if let Some(v) = non_empty_env("ISOL8_AUTO_PROFILES") {
        cfg.auto_profiles = parse_bool(&v);
    }
    if matches!(
        std::env::var("ISOL8_DRY_RUN").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    ) {
        cfg.dry_run = true;
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// `1` / `true` / `yes` / `on` → true.
pub fn parse_bool(s: &str) -> bool {
    matches!(s, "1" | "true" | "yes" | "on")
}

/// Split a list-valued env var on `,` or `:`, trimming and dropping empties.
pub fn split_list(s: &str) -> Vec<String> {
    s.split([',', ':'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// @init template
// ---------------------------------------------------------------------------

/// Default config file content for `isol8 @init`.
pub fn init_template(format: &str) -> Result<String> {
    let defaults = Config::builtin_defaults();
    match format {
        "yaml" | "yml" => {
            let profiles_yaml = defaults
                .default_profiles
                .iter()
                .map(|p| format!("  - {p}"))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(format!(
                r#"# isol8 configuration
default_profiles:
{profiles_yaml}
auto_profiles: {auto}
profile_paths: []
# profile_paths:
#   - /path/to/extra-profiles
#   - @/profiles   # relative to this config directory
# cage: work   # optional named cage (~/.config/isol8/cages/work.toml)
add_dirs_rw: []
add_dirs_ro: []
"#,
                auto = defaults.auto_profiles,
            ))
        }
        _ => Ok(format!(
            r#"# isol8 configuration
default_profiles = {dp:?}
auto_profiles = {auto}
profile_paths = []
# profile_paths = ["/path/to/extra-profiles", "@/profiles"]
# cage = "work"  # optional named cage (~/.config/isol8/cages/work.toml)
# Paths starting with @ are relative to this config directory.
add_dirs_rw = []
add_dirs_ro = []
"#,
            dp = defaults.default_profiles,
            auto = defaults.auto_profiles,
        )),
    }
}

/// Where `isol8 @init` writes by default.
pub fn default_init_path(format: &str) -> PathBuf {
    let ext = if format == "yaml" || format == "yml" {
        "yaml"
    } else {
        "toml"
    };
    os_config_dir().join(format!("isol8.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builtin_defaults_include_system_runtime() {
        let d = Config::builtin_defaults();
        assert!(d.default_profiles.contains(&"base".to_string()));
        assert_eq!(d.default_profiles.len(), 2);
    }

    #[test]
    fn split_list_comma_and_colon() {
        assert_eq!(
            split_list("a,b:c"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn expand_at_path_relative_to_config_dir() {
        let dir = PathBuf::from("/cfg/isol8");
        assert_eq!(
            expand_at_path("@/profiles", &dir),
            PathBuf::from("/cfg/isol8/profiles").display().to_string()
        );
        assert_eq!(
            expand_at_path("@profiles", &dir),
            PathBuf::from("/cfg/isol8/profiles").display().to_string()
        );
        assert_eq!(expand_at_path("@", &dir), dir.display().to_string());
        assert_eq!(expand_at_path("/abs", &dir), "/abs");
        let rel = expand_at_path("rel", &dir);
        assert!(
            PathBuf::from(&rel).is_absolute(),
            "expected absolute, got {rel}"
        );
        assert!(rel.ends_with("rel"), "{rel}");
    }

    #[test]
    fn raw_to_base_empty_profiles_uses_builtin() {
        let raw = RawConfig {
            default_profiles: Some(vec![]),
            auto_profiles: Some(false),
            ..Default::default()
        };
        let cfg = raw_to_base(raw);
        assert_eq!(
            cfg.default_profiles,
            Config::builtin_defaults().default_profiles
        );
        assert!(!cfg.auto_profiles);
    }

    #[test]
    fn merge_overlay_local_wins_partial() {
        let base = Config {
            auto_profiles: true,
            home: Some("/global-home".into()),
            add_dirs_rw: vec!["/a".into()],
            ..Config::builtin_defaults()
        };
        let overlay = RawConfig {
            auto_profiles: Some(false),
            home: Some("/local-home".into()),
            // add_dirs_rw absent → keep base
            ..Default::default()
        };
        let merged = merge_overlay(base, overlay);
        assert!(!merged.auto_profiles);
        assert_eq!(merged.home.as_deref(), Some("/local-home"));
        assert_eq!(merged.add_dirs_rw, vec!["/a".to_string()]);
    }

    fn test_tmp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "isol8-cfg-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_config_path_redirect_and_overlay() {
        let tmp = test_tmp("redirect");
        let global = tmp.join("global");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("isol8.toml"),
            "auto_profiles = true\nadd_dirs_rw = [\"@/data\"]\nhome = \"@/homes/g\"\n",
        )
        .unwrap();

        let proj = tmp.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let marker = proj.join(".isol8.toml");
        std::fs::write(
            &marker,
            format!(
                "config_path = \"{}\"\nauto_profiles = false\ncage = \"work\"\n",
                global.display()
            ),
        )
        .unwrap();

        // Hermetic: pass the marker explicitly instead of chdir-ing, so this test
        // cannot race the cwd-sensitive tests sharing this binary.
        let cfg = load_with_marker(Some(marker), &tmp.join("unused-os-dir")).expect("load");
        assert!(!cfg.auto_profiles);
        assert_eq!(cfg.cage.as_deref(), Some("work"));
        let data = context::absolute_path(&global.join("data"));
        let home = context::absolute_path(&global.join("homes/g"));
        assert_eq!(cfg.add_dirs_rw, vec![data.display().to_string()]);
        assert_eq!(
            cfg.home.as_deref(),
            Some(home.display().to_string().as_str())
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_ignore_global_skips_os_base() {
        let tmp = test_tmp("ignore");
        let proj = tmp.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let marker = proj.join("encage.toml");
        std::fs::write(
            &marker,
            "ignore_global = true\nauto_profiles = false\ndefault_profiles = [\"base\"]\n",
        )
        .unwrap();

        // An OS base that *would* conflict — `ignore_global` must skip it.
        let os_dir = tmp.join("osdir");
        std::fs::create_dir_all(&os_dir).unwrap();
        std::fs::write(
            os_dir.join("isol8.toml"),
            "auto_profiles = true\ndefault_profiles = [\"base\", \"macos/system-runtime\"]\n",
        )
        .unwrap();

        let cfg = load_with_marker(Some(marker), &os_dir).expect("load");
        assert!(!cfg.auto_profiles);
        assert_eq!(cfg.default_profiles, vec!["base".to_string()]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn os_base_used_when_no_marker() {
        let tmp = test_tmp("osbase");
        std::fs::write(
            tmp.join("isol8.toml"),
            "auto_profiles = false\ndefault_profiles = [\"base\"]\n",
        )
        .unwrap();
        let cfg = load_with_marker(None, &tmp).expect("load");
        assert!(!cfg.auto_profiles);
        assert_eq!(cfg.default_profiles, vec!["base".to_string()]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_config_location_dir_and_file() {
        let tmp = test_tmp("resolve");
        let f = tmp.join("isol8.toml");
        std::fs::write(&f, "auto_profiles = true\n").unwrap();
        assert_eq!(resolve_config_location(&f).as_deref(), Some(f.as_path()));
        assert_eq!(resolve_config_location(&tmp).as_deref(), Some(f.as_path()));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn registries_kept_as_raw_tables_and_path_expanded() {
        let tmp = test_tmp("regs");
        std::fs::write(
            tmp.join("isol8.toml"),
            "auto_profiles = true\n\n[registries.scratch]\npath = \"@/registries/scratch\"\n",
        )
        .unwrap();

        let prev_env = std::env::var_os("ISOL8_CONFIG_PATH");
        std::env::set_var("ISOL8_CONFIG_PATH", &tmp);
        let cfg = load().expect("load");
        match prev_env {
            Some(v) => std::env::set_var("ISOL8_CONFIG_PATH", v),
            None => std::env::remove_var("ISOL8_CONFIG_PATH"),
        }

        let scratch = cfg.registries.get("scratch").expect("scratch registry");
        let path = scratch.get("path").and_then(|v| v.as_str()).unwrap();
        assert!(Path::new(path).is_absolute(), "{path}");
        assert!(path.ends_with("registries/scratch"), "{path}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
