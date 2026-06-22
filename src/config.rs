//! Global isol8 config file discovery, parsing, and env-var overrides.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::cli::RunArgs;

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
    pub dry_run: bool,
}

impl Config {
    /// OS-specific defaults used by `isol8 init` and when no config file exists.
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

/// Resolved config file path (if any).
pub fn discover_config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ISOL8_CONFIG_PATH") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
        if p.is_dir() {
            for name in ["isol8.toml", "isol8.yaml", "isol8.yml"] {
                let candidate = p.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    for name in ["isol8.toml", "isol8.yaml", "isol8.yml"] {
        let candidate = PathBuf::from(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
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
    for name in ["isol8.toml", "isol8.yaml", "isol8.yml"] {
        let candidate = config_home.join("isol8").join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn load() -> Result<Config> {
    let Some(path) = discover_config_path() else {
        return Ok(Config::builtin_defaults());
    };
    load_from(&path)
}

fn load_from(path: &Path) -> Result<Config> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("reading config '{}'", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut cfg: Config = match ext {
        "yaml" | "yml" => serde_yaml::from_str(&body)
            .with_context(|| format!("parsing YAML config '{}'", path.display()))?,
        _ => toml::from_str(&body)
            .with_context(|| format!("parsing TOML config '{}'", path.display()))?,
    };
    // Empty default_profiles in file → use builtin OS defaults.
    if cfg.default_profiles.is_empty() {
        cfg.default_profiles = Config::builtin_defaults().default_profiles;
    }
    Ok(cfg)
}

/// Apply config defaults to `run` (only fills unset CLI fields).
///
/// `cli_auto_profiles`: when `Some`, the user set `--auto-profiles` or
/// `--no-auto-profiles` and that choice wins over config/env.
pub fn apply_to_run(cfg: &Config, run: &mut RunArgs, cli_auto_profiles: Option<bool>) {
    if run.profiles().is_empty() {
        run.opts.profiles = cfg.default_profiles.clone();
    }
    if cli_auto_profiles.is_none() {
        run.opts.auto_profiles = cfg.auto_profiles;
    }
    if run.profile_paths().is_empty() {
        run.opts.profile_paths = cfg.profile_paths.clone();
    }
    if run.add_dirs_rw().is_empty() {
        run.opts.add_dirs_rw = cfg.add_dirs_rw.clone();
    }
    if run.add_dirs_ro().is_empty() {
        run.opts.add_dirs_ro = cfg.add_dirs_ro.clone();
    }
    if run.home().is_none() {
        run.opts.home = cfg.home.clone();
    }
    if !run.dry_run() {
        run.opts.dry_run = cfg.dry_run;
    }
}

/// Apply `ISOL8_*` env overrides (between config and CLI in precedence).
///
/// When `cli_auto_profiles_set` is true, `ISOL8_AUTO_PROFILES` is ignored.
pub fn apply_env_overrides(run: &mut RunArgs, cli_auto_profiles_set: bool) {
    if let Ok(v) = std::env::var("ISOL8_PROFILE") {
        if !v.is_empty() {
            run.opts.profiles = split_list(&v);
        }
    }
    if let Ok(v) = std::env::var("ISOL8_PROFILE_PATH") {
        if !v.is_empty() {
            run.opts.profile_paths = split_list(&v);
        }
    }
    if let Ok(v) = std::env::var("ISOL8_ADD_DIRS_RW") {
        if !v.is_empty() {
            run.opts.add_dirs_rw = split_list(&v);
        }
    }
    if let Ok(v) = std::env::var("ISOL8_ADD_DIRS_RO") {
        if !v.is_empty() {
            run.opts.add_dirs_ro = split_list(&v);
        }
    }
    if let Ok(v) = std::env::var("ISOL8_HOME") {
        if !v.is_empty() {
            run.opts.home = Some(v);
        }
    }
    if !cli_auto_profiles_set {
        if let Ok(v) = std::env::var("ISOL8_AUTO_PROFILES") {
            if !v.is_empty() {
                run.opts.auto_profiles = parse_bool(&v);
            }
        }
    }
    if matches!(
        std::env::var("ISOL8_DRY_RUN").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    ) {
        run.opts.dry_run = true;
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

/// Default config file content for `isol8 init`.
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
# profile_paths = ["/path/to/extra-profiles", "/path/to/override.toml"]
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
        .map(|p| p.join("isol8").join(format!("isol8.{ext}")))
        .unwrap_or_else(|| PathBuf::from(format!("isol8.{ext}")))
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
        let mut run = crate::cli::run_from(Default::default(), vec!["echo".into()]);
        apply_to_run(&cfg, &mut run, None);
        assert!(!run.auto_profiles());
    }

    #[test]
    fn env_auto_profiles_overrides_config() {
        let cfg = Config {
            auto_profiles: false,
            ..Config::builtin_defaults()
        };
        let prev = std::env::var_os("ISOL8_AUTO_PROFILES");
        std::env::set_var("ISOL8_AUTO_PROFILES", "true");

        let mut run = crate::cli::run_from(Default::default(), vec!["echo".into()]);
        apply_to_run(&cfg, &mut run, None);
        apply_env_overrides(&mut run, false);
        assert!(run.auto_profiles());

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
        let mut run = crate::cli::run_from(
            crate::cli::ProfileOpts {
                no_auto_profiles: true,
                ..Default::default()
            },
            vec!["echo".into()],
        );
        let cli_auto = run.opts.auto_profiles_cli_override();
        apply_to_run(&cfg, &mut run, cli_auto);
        apply_env_overrides(&mut run, cli_auto.is_some());
        if let Some(v) = cli_auto {
            run.opts.auto_profiles = v;
        }
        assert!(!run.auto_profiles());
    }
}
