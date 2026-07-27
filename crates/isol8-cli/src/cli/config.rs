//! Global isol8 config file discovery, parsing, and env-var overrides.
//!
//! # Discovery order
//!
//! 1. `ISOL8_CONFIG_PATH` (file, or directory containing `isol8.toml` / yaml) —
//!    absolute override; no local merge.
//! 2. Project-local marker in cwd (first match wins):
//!    `isol8.toml`, `.isol8.toml`, `encage.toml`, `.encage.toml`
//!    - optional `config_path = "…"` redirects the global/base config (same as env)
//!    - optional `ignore_global = true` skips OS / redirected base entirely
//!    - other fields merge onto the base (local wins)
//! 3. OS default: `$XDG_CONFIG_HOME/isol8/` or `~/.config/isol8/` (Windows: `%APPDATA%/isol8/`)
//!
//! # Path tokens
//!
//! Paths in config that start with `@` are resolved relative to the **config
//! directory** (parent of the base config file, or the local marker's parent
//! when there is no base file). Example: `profile_paths = ["@/profiles"]`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::cli::ProfileOpts;
use isol8_registry::{self, RegistrySpec};

/// Project-local config markers (cwd). First existing file wins.
pub const PROJECT_CONFIG_MARKERS: &[&str] =
    &["isol8.toml", ".isol8.toml", "encage.toml", ".encage.toml"];

/// Canonical config basenames inside a config directory.
const CONFIG_BASENAMES: &[&str] = &["isol8.toml", "isol8.yaml", "isol8.yml"];

/// User-facing config (isol8.toml / isol8.yaml).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub default_profiles: Vec<String>,
    pub auto_profiles: bool,
    pub profile_paths: Vec<String>,
    pub add_dirs_rw: Vec<String>,
    pub add_dirs_ro: Vec<String>,
    pub home: Option<String>,
    /// Named cage to use when `--cage` / `ISOL8_CAGE` are unset.
    pub cage: Option<String>,
    pub dry_run: bool,
    /// Named recipe registries (`[registries.<name>]`). Parsed separately from
    /// free-form TOML tables so path/git/url shapes stay flexible.
    #[serde(default, skip_deserializing)]
    pub registries: BTreeMap<String, RegistrySpec>,
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
    /// Redirect the global/base config (file or directory). Like `ISOL8_CONFIG_PATH`.
    pub config_path: Option<String>,
    /// When true, do not load or merge the OS / redirected global config.
    pub ignore_global: bool,
    pub default_profiles: Option<Vec<String>>,
    pub auto_profiles: Option<bool>,
    pub profile_paths: Option<Vec<String>>,
    pub add_dirs_rw: Option<Vec<String>>,
    pub add_dirs_ro: Option<Vec<String>>,
    pub home: Option<String>,
    pub cage: Option<String>,
    pub dry_run: Option<bool>,
    #[serde(default, skip_deserializing)]
    pub registries: BTreeMap<String, RegistrySpec>,
}

/// OS config directory for isol8 (`~/.config/isol8` on macOS/Linux).
pub fn os_config_dir() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
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
        })?;
    Some(config_home.join("isol8"))
}

/// First project-local marker in `cwd` (relative paths as returned).
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
        for name in CONFIG_BASENAMES {
            let candidate = path.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_config_in_dir(dir: &Path) -> Option<PathBuf> {
    for name in CONFIG_BASENAMES {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Expand `@…` paths relative to `config_dir`. Non-`@` paths are unchanged.
pub fn expand_at_path(path: &str, config_dir: &Path) -> String {
    let Some(rest) = path.strip_prefix('@') else {
        return path.to_string();
    };
    let rest = rest
        .strip_prefix('/')
        .or_else(|| rest.strip_prefix('\\'))
        .unwrap_or(rest);
    if rest.is_empty() {
        return config_dir.display().to_string();
    }
    config_dir.join(rest).display().to_string()
}

fn expand_at_paths(paths: &mut [String], config_dir: &Path) {
    for p in paths.iter_mut() {
        *p = expand_at_path(p, config_dir);
    }
}

fn expand_config_paths(cfg: &mut Config, config_dir: &Path) {
    expand_at_paths(&mut cfg.profile_paths, config_dir);
    expand_at_paths(&mut cfg.add_dirs_rw, config_dir);
    expand_at_paths(&mut cfg.add_dirs_ro, config_dir);
    if let Some(ref home) = cfg.home {
        cfg.home = Some(expand_at_path(home, config_dir));
    }
    for spec in cfg.registries.values_mut() {
        if let RegistrySpec::Path { path, .. } = spec {
            *path = expand_at_path(path, config_dir);
        }
    }
}

/// Load config using discovery rules (env → local marker → OS default).
pub fn load() -> Result<Config> {
    // 1. Explicit env override — no local merge.
    if let Ok(path) = std::env::var("ISOL8_CONFIG_PATH") {
        let p = PathBuf::from(&path);
        let Some(file) = resolve_config_location(&p) else {
            anyhow::bail!(
                "ISOL8_CONFIG_PATH='{}' is not a config file or a directory containing isol8.toml/yaml",
                path
            );
        };
        let mut cfg = load_file_as_base(&file)?;
        let dir = file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        expand_config_paths(&mut cfg, &dir);
        return Ok(cfg);
    }

    let local = discover_local_marker();
    let local_raw = match &local {
        Some(path) => Some(load_raw(path)?),
        None => None,
    };

    // 2+3. Base = config_path redirect | OS default | none (ignore_global / missing).
    let (base, config_dir) = if let Some(raw) = &local_raw {
        if raw.ignore_global {
            let dir = local
                .as_ref()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .filter(|d| !d.as_os_str().is_empty())
                .unwrap_or_else(|| PathBuf::from("."));
            (Config::builtin_defaults(), dir)
        } else if let Some(cp) = raw.config_path.as_deref().filter(|s| !s.is_empty()) {
            let loc = PathBuf::from(cp);
            match resolve_config_location(&loc) {
                Some(file) => {
                    let dir = file
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from("."));
                    (load_file_as_base(&file)?, dir)
                }
                None => anyhow::bail!(
                    "config_path='{}' in {} is not a config file or directory containing isol8.toml/yaml",
                    cp,
                    local.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
                ),
            }
        } else if let Some(os_dir) = os_config_dir() {
            match find_config_in_dir(&os_dir) {
                Some(file) => {
                    let dir = file.parent().map(Path::to_path_buf).unwrap_or(os_dir);
                    (load_file_as_base(&file)?, dir)
                }
                None => (Config::builtin_defaults(), os_dir),
            }
        } else {
            (Config::builtin_defaults(), PathBuf::from("."))
        }
    } else if let Some(os_dir) = os_config_dir() {
        match find_config_in_dir(&os_dir) {
            Some(file) => {
                let dir = file.parent().map(Path::to_path_buf).unwrap_or(os_dir);
                (load_file_as_base(&file)?, dir)
            }
            None => (Config::builtin_defaults(), os_dir),
        }
    } else {
        (Config::builtin_defaults(), PathBuf::from("."))
    };

    let mut cfg = if let Some(raw) = local_raw {
        merge_overlay(base, raw)
    } else {
        base
    };
    expand_config_paths(&mut cfg, &config_dir);
    Ok(cfg)
}

fn load_raw(path: &Path) -> Result<RawConfig> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("reading config '{}'", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("toml");
    let (body_without_regs, registries) = strip_and_parse_registries(&body, ext)?;
    let mut raw: RawConfig = match ext {
        "yaml" | "yml" => serde_yaml::from_str(&body_without_regs)
            .with_context(|| format!("parsing YAML config '{}'", path.display()))?,
        _ => toml::from_str(&body_without_regs)
            .with_context(|| format!("parsing TOML config '{}'", path.display()))?,
    };
    raw.registries = registries;
    Ok(raw)
}

fn load_file_as_base(path: &Path) -> Result<Config> {
    let raw = load_raw(path)?;
    Ok(raw_to_base(raw))
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

/// Remove registries from the body (so deny_unknown_fields succeeds) and parse them.
fn strip_and_parse_registries(
    body: &str,
    ext: &str,
) -> Result<(String, BTreeMap<String, RegistrySpec>)> {
    if matches!(ext, "yaml" | "yml") {
        // YAML: parse full doc, pull registries, re-serialize without them.
        // Simpler path: if YAML contains registries, parse via toml-less map.
        // For Phase 7, registries are TOML-first; YAML configs rarely use them.
        let mut value: serde_yaml::Value = serde_yaml::from_str(body)
            .with_context(|| "parsing YAML for registries".to_string())?;
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
                        // Convert YAML value → TOML via JSON intermediate for reuse.
                        let json = serde_json::to_value(v)
                            .with_context(|| format!("registries.{name}: converting YAML entry"))?;
                        let toml_v: toml::Value =
                            serde_json::from_value(json).with_context(|| {
                                format!("registries.{name}: converting to TOML value")
                            })?;
                        regs.insert(
                            name.clone(),
                            isol8_registry::parse_registry_spec(&name, &toml_v)
                                .map_err(|e| anyhow::anyhow!("{e}"))?,
                        );
                    }
                }
            }
        }
        let stripped = serde_yaml::to_string(&value)
            .with_context(|| "re-serializing YAML without registries".to_string())?;
        return Ok((stripped, regs));
    }

    // TOML: parse full document once for registries; rebuild body without that table.
    let value: toml::Value =
        toml::from_str(body).with_context(|| "parsing TOML for registries".to_string())?;
    let regs =
        isol8_registry::parse_registries_from_toml(body).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut table = value
        .as_table()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("config root must be a table"))?;
    table.remove("registries");
    let stripped = toml::to_string(&toml::Value::Table(table))
        .with_context(|| "re-serializing TOML without registries".to_string())?;
    Ok((stripped, regs))
}

/// Apply config defaults to `run` (only fills unset CLI fields).
///
/// `cli_auto_profiles`: when `Some`, the user set `--auto-profiles` or
/// `--no-auto-profiles` and that choice wins over config/env.
pub fn apply_to_run(cfg: &Config, opts: &mut ProfileOpts, cli_auto_profiles: Option<bool>) {
    if opts.profiles.is_empty() {
        opts.profiles = cfg.default_profiles.clone();
    }
    if cli_auto_profiles.is_none() {
        opts.auto_profiles = cfg.auto_profiles;
    }
    if opts.profile_paths.is_empty() {
        opts.profile_paths = cfg.profile_paths.clone();
    }
    if opts.add_dirs_rw.is_empty() {
        opts.add_dirs_rw = cfg.add_dirs_rw.clone();
    }
    if opts.add_dirs_ro.is_empty() {
        opts.add_dirs_ro = cfg.add_dirs_ro.clone();
    }
    if opts.home.is_none() {
        opts.home = cfg.home.clone();
    }
    if !(opts.show_policies || opts.dry_run) {
        opts.dry_run = cfg.dry_run;
    }
}

/// Apply `ISOL8_*` env overrides (between config and CLI in precedence).
///
/// When `cli_auto_profiles_set` is true, `ISOL8_AUTO_PROFILES` is ignored.
pub fn apply_env_overrides(opts: &mut ProfileOpts, cli_auto_profiles_set: bool) {
    if let Ok(v) = std::env::var("ISOL8_PROFILE") {
        if !v.is_empty() {
            opts.profiles = split_list(&v);
        }
    }
    if let Ok(v) = std::env::var("ISOL8_PROFILE_PATH") {
        if !v.is_empty() {
            opts.profile_paths = split_list(&v);
        }
    }
    if let Ok(v) = std::env::var("ISOL8_ADD_DIRS_RW") {
        if !v.is_empty() {
            opts.add_dirs_rw = split_list(&v);
        }
    }
    if let Ok(v) = std::env::var("ISOL8_ADD_DIRS_RO") {
        if !v.is_empty() {
            opts.add_dirs_ro = split_list(&v);
        }
    }
    if let Ok(v) = std::env::var("ISOL8_HOME") {
        if !v.is_empty() {
            opts.home = Some(v);
        }
    }
    if !cli_auto_profiles_set {
        if let Ok(v) = std::env::var("ISOL8_AUTO_PROFILES") {
            if !v.is_empty() {
                opts.auto_profiles = parse_bool(&v);
            }
        }
    }
    if matches!(
        std::env::var("ISOL8_DRY_RUN").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    ) {
        opts.dry_run = true;
    }
}

fn parse_bool(s: &str) -> bool {
    matches!(s, "1" | "true" | "yes" | "on")
}

fn split_list(s: &str) -> Vec<String> {
    s.split([',', ':'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

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

pub fn default_init_path(format: &str) -> PathBuf {
    let ext = if format == "yaml" || format == "yml" {
        "yaml"
    } else {
        "toml"
    };
    os_config_dir()
        .map(|p| p.join(format!("isol8.{ext}")))
        .unwrap_or_else(|| PathBuf::from(format!("isol8.{ext}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize tests that touch process-global cwd / env.
    static CWD_LOCK: Mutex<()> = Mutex::new(());

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
    fn init_template_yaml_is_valid_yaml() {
        let body = init_template("yaml").unwrap();
        let cfg: Config = serde_yaml::from_str(&body).unwrap();
        assert!(cfg.auto_profiles);
        assert!(!cfg.default_profiles.is_empty());
    }

    #[test]
    fn apply_to_run_respects_config_auto_profiles_false() {
        let cfg = Config {
            auto_profiles: false,
            ..Config::builtin_defaults()
        };
        let mut opts = ProfileOpts::default();
        apply_to_run(&cfg, &mut opts, None);
        assert!(!opts.auto_profiles);
    }

    #[test]
    fn env_auto_profiles_overrides_config() {
        let cfg = Config {
            auto_profiles: false,
            ..Config::builtin_defaults()
        };
        let prev = std::env::var_os("ISOL8_AUTO_PROFILES");
        std::env::set_var("ISOL8_AUTO_PROFILES", "true");

        let mut opts = ProfileOpts::default();
        apply_to_run(&cfg, &mut opts, None);
        apply_env_overrides(&mut opts, false);
        assert!(opts.auto_profiles);

        match prev {
            Some(v) => std::env::set_var("ISOL8_AUTO_PROFILES", v),
            None => std::env::remove_var("ISOL8_AUTO_PROFILES"),
        }
    }

    #[test]
    fn cli_no_auto_profiles_overrides_config() {
        let cfg = Config {
            auto_profiles: true,
            ..Config::builtin_defaults()
        };
        let mut opts = ProfileOpts {
            no_auto_profiles: true,
            ..Default::default()
        };
        let cli_auto = opts.auto_profiles_cli_override();
        apply_to_run(&cfg, &mut opts, cli_auto);
        apply_env_overrides(&mut opts, cli_auto.is_some());
        if let Some(v) = cli_auto {
            opts.auto_profiles = v;
        }
        assert!(!opts.auto_profiles);
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
        assert_eq!(expand_at_path("rel", &dir), "rel");
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
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = test_tmp("redirect");
        let global = tmp.join("global");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("isol8.toml"),
            r#"
auto_profiles = true
add_dirs_rw = ["@/data"]
home = "@/homes/g"
"#,
        )
        .unwrap();

        let proj = tmp.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join(".isol8.toml"),
            format!(
                r#"
config_path = "{}"
auto_profiles = false
cage = "work"
"#,
                global.display()
            ),
        )
        .unwrap();

        let prev_cwd = std::env::current_dir().unwrap();
        let prev_env = std::env::var_os("ISOL8_CONFIG_PATH");
        std::env::remove_var("ISOL8_CONFIG_PATH");
        std::env::set_current_dir(&proj).unwrap();

        let cfg = load().expect("load");
        assert!(!cfg.auto_profiles);
        assert_eq!(cfg.cage.as_deref(), Some("work"));
        assert_eq!(
            cfg.add_dirs_rw,
            vec![global.join("data").display().to_string()]
        );
        assert_eq!(
            cfg.home.as_deref(),
            Some(global.join("homes/g").display().to_string().as_str())
        );

        std::env::set_current_dir(prev_cwd).unwrap();
        match prev_env {
            Some(v) => std::env::set_var("ISOL8_CONFIG_PATH", v),
            None => std::env::remove_var("ISOL8_CONFIG_PATH"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_ignore_global_skips_os_and_redirect() {
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = test_tmp("ignore");
        let proj = tmp.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("encage.toml"),
            r#"
ignore_global = true
auto_profiles = false
default_profiles = ["base"]
"#,
        )
        .unwrap();

        let prev_cwd = std::env::current_dir().unwrap();
        let prev_env = std::env::var_os("ISOL8_CONFIG_PATH");
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::remove_var("ISOL8_CONFIG_PATH");
        // Point XDG at a dir with a conflicting global config — must be ignored.
        let xdg = tmp.join("xdg");
        std::fs::create_dir_all(xdg.join("isol8")).unwrap();
        std::fs::write(
            xdg.join("isol8/isol8.toml"),
            r#"auto_profiles = true
default_profiles = ["base", "macos/system-runtime"]
"#,
        )
        .unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
        std::env::set_current_dir(&proj).unwrap();

        let cfg = load().expect("load");
        assert!(!cfg.auto_profiles);
        assert_eq!(cfg.default_profiles, vec!["base".to_string()]);

        std::env::set_current_dir(prev_cwd).unwrap();
        match prev_env {
            Some(v) => std::env::set_var("ISOL8_CONFIG_PATH", v),
            None => std::env::remove_var("ISOL8_CONFIG_PATH"),
        }
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
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
}
