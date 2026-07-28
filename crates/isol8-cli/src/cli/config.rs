//! Clap glue between [`isol8_core::config`] and [`ProfileOpts`].
//!
//! Discovery, parsing, merge, `@` expansion, `ISOL8_*` overrides, and the
//! `@init` template all live in `isol8-core` — this module only maps a loaded
//! [`Config`] onto the clap-derived options struct. See
//! [`_docs/config.md`](../../../../_docs/config.md).

pub use isol8_core::config::Config;

use crate::cli::ProfileOpts;

// Re-exported so `cli::config::…` call sites (and any embedder that reached for
// them here) keep working against the single core implementation.
#[allow(unused_imports)]
pub use isol8_core::config::{
    apply_env_overrides, default_init_path, discover_config_file, discover_local_marker,
    effective_cages_dir, effective_config_dir, expand_at_path, init_template, load, load_in,
    os_config_dir, resolve_config_location, CONFIG_BASENAMES, PROJECT_CONFIG_MARKERS,
};

/// Apply config defaults to `opts` — fills **only** fields the CLI left unset.
///
/// Precedence ([`_docs/config.md`](../../../../_docs/config.md) §7): the config
/// has already absorbed the project marker overlay and the `ISOL8_*` overrides,
/// so anything the user typed on the command line wins here.
///
/// `cli_auto_profiles`: `Some` when the user passed `--auto-profiles` /
/// `--no-auto-profiles`; that choice wins over config and env.
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn cli_profiles_win_over_config() {
        let cfg = Config::builtin_defaults();
        let mut opts = ProfileOpts {
            profiles: vec!["toolchains/rust".into()],
            ..Default::default()
        };
        apply_to_run(&cfg, &mut opts, None);
        assert_eq!(opts.profiles, vec!["toolchains/rust".to_string()]);
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
        if let Some(v) = cli_auto {
            opts.auto_profiles = v;
        }
        assert!(!opts.auto_profiles);
    }

    #[test]
    fn env_overrides_config_but_loses_to_cli() {
        let mut cfg = Config::builtin_defaults();
        let prev = std::env::var_os("ISOL8_PROFILE");
        std::env::set_var("ISOL8_PROFILE", "base");
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.default_profiles, vec!["base".to_string()]);

        // A CLI-set profile is not clobbered by the env var.
        let mut opts = ProfileOpts {
            profiles: vec!["toolchains/rust".into()],
            ..Default::default()
        };
        apply_to_run(&cfg, &mut opts, None);
        assert_eq!(opts.profiles, vec!["toolchains/rust".to_string()]);

        match prev {
            Some(v) => std::env::set_var("ISOL8_PROFILE", v),
            None => std::env::remove_var("ISOL8_PROFILE"),
        }
    }

    #[test]
    fn init_template_yaml_is_valid_yaml() {
        let body = init_template("yaml").unwrap();
        let cfg: Config = serde_yaml::from_str(&body).unwrap();
        assert!(cfg.auto_profiles);
        assert!(!cfg.default_profiles.is_empty());
    }
}
