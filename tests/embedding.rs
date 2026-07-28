//! Integration coverage for the embedding API surface: hermetic config/context
//! loading, the CLI-flag/cage/config precedence chain, and the `--json` wire
//! contract (`DryRun` + its enum spellings). See `AGENTS.md` §"embedding".
//!
//! Tests that mutate process env (`ISOL8_*`) are serialized behind `ENV_LOCK`
//! and restore whatever value (if any) was there before, mirroring the pattern
//! in `crates/isol8-core/src/env.rs` and `crates/isol8-cli/src/cli/config.rs`.

use isol8::{analyze, cage, config, resolve, sandbox};
use isol8::{
    CageOverlay, Config, Context, HomeOpKind, LayerOrigin, PlanAction, Platform, Spec,
    StrategyName, ToolchainChoice,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn tmp_dir(label: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "isol8-embedding-{label}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// config::load_in — hermetic discovery, no ambient env reads
// ---------------------------------------------------------------------------

#[test]
fn load_in_ignores_process_env_and_applies_marker_overlay() {
    let _g = ENV_LOCK.lock().unwrap();

    let root = tmp_dir("load-in-hermetic");
    let config_dir = root.join("cfg");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("isol8.toml"),
        "auto_profiles = true\ndefault_profiles = [\"cfgprofile\"]\n",
    )
    .unwrap();

    let cwd = root.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join(".isol8.toml"), "auto_profiles = false\n").unwrap();

    // A decoy config `load_in` must NOT read — proves it reads no env vars.
    let decoy_dir = root.join("decoy");
    std::fs::create_dir_all(&decoy_dir).unwrap();
    std::fs::write(
        decoy_dir.join("isol8.toml"),
        "default_profiles = [\"envprofile\"]\n",
    )
    .unwrap();

    let prev_path = std::env::var_os("ISOL8_CONFIG_PATH");
    let prev_profile = std::env::var_os("ISOL8_PROFILE");
    std::env::set_var("ISOL8_CONFIG_PATH", &decoy_dir);
    std::env::set_var("ISOL8_PROFILE", "should-not-be-read");

    let ctx = Context {
        real_home: root.join("home"),
        cwd: cwd.clone(),
        platform: Platform::current(),
        config_dir: config_dir.clone(),
        managed_root: config_dir.join("homes"),
    };
    let result = config::load_in(&ctx);

    match prev_path {
        Some(v) => std::env::set_var("ISOL8_CONFIG_PATH", v),
        None => std::env::remove_var("ISOL8_CONFIG_PATH"),
    }
    match prev_profile {
        Some(v) => std::env::set_var("ISOL8_PROFILE", v),
        None => std::env::remove_var("ISOL8_PROFILE"),
    }

    let cfg = result.expect("load_in");

    // Base came from `ctx.config_dir`, not the decoy `ISOL8_CONFIG_PATH`.
    assert_eq!(cfg.default_profiles, vec!["cfgprofile".to_string()]);
    // The `.isol8.toml` marker under `ctx.cwd` overlaid onto that base.
    assert!(!cfg.auto_profiles);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn load_in_expands_at_paths_under_config_dir() {
    let root = tmp_dir("load-in-atexpand");
    let config_dir = root.join("cfg");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("isol8.toml"),
        "profile_paths = [\"@/profiles\"]\nadd_dirs_rw = [\"@/data\"]\n",
    )
    .unwrap();

    let cwd = root.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();

    let ctx = Context {
        real_home: root.join("home"),
        cwd,
        platform: Platform::current(),
        config_dir: config_dir.clone(),
        managed_root: config_dir.join("homes"),
    };
    let cfg = config::load_in(&ctx).expect("load_in");

    assert_eq!(
        cfg.profile_paths,
        vec![config_dir.join("profiles").display().to_string()]
    );
    assert_eq!(
        cfg.add_dirs_rw,
        vec![config_dir.join("data").display().to_string()]
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn apply_env_overrides_sets_default_profiles_from_env() {
    let _g = ENV_LOCK.lock().unwrap();
    let prev = std::env::var_os("ISOL8_PROFILE");
    std::env::set_var("ISOL8_PROFILE", "custom-a,custom-b");

    let mut cfg = Config::builtin_defaults();
    config::apply_env_overrides(&mut cfg);

    match prev {
        Some(v) => std::env::set_var("ISOL8_PROFILE", v),
        None => std::env::remove_var("ISOL8_PROFILE"),
    }

    assert_eq!(
        cfg.default_profiles,
        vec!["custom-a".to_string(), "custom-b".to_string()]
    );
}

// ---------------------------------------------------------------------------
// resolve::spec_from_config — CLI flag > cage > config precedence
// ---------------------------------------------------------------------------

#[test]
fn spec_from_config_preset_beats_cage_beats_config() {
    let _g = ENV_LOCK.lock().unwrap();
    let prev_cage_env = std::env::var_os("ISOL8_CAGE");
    std::env::remove_var("ISOL8_CAGE");

    let root = tmp_dir("spec-from-config");
    let cwd = root.join("proj");
    let cages_dir = cwd.join(".isol8").join("cages");
    std::fs::create_dir_all(&cages_dir).unwrap();
    std::fs::write(
        cages_dir.join("mycage.toml"),
        r#"
schema = 1
name = "mycage"
home = "/cage-home"
profiles = ["cage-profile"]
[[dirs]]
path = "/cage-rw"
access = "rw"
"#,
    )
    .unwrap();

    let config_dir = root.join("cfg");
    std::fs::create_dir_all(&config_dir).unwrap();

    let ctx = Context {
        real_home: root.join("home"),
        cwd,
        platform: Platform::current(),
        config_dir: config_dir.clone(),
        managed_root: config_dir.join("homes"),
    };

    let cfg = Config {
        default_profiles: vec!["cfg-profile".into()],
        add_dirs_rw: vec!["/cfg-rw".into()],
        add_dirs_ro: vec!["/cfg-ro".into()],
        home: Some("/cfg-home".into()),
        cage: Some("mycage".into()),
        ..Config::builtin_defaults()
    };

    let mut base = Spec::default();
    base.profiles = vec!["cli-profile".into()]; // pre-set: models a CLI flag
    base.add_dirs_rw = vec!["/cli-rw".into()]; // pre-set: models a CLI flag

    let result = resolve::spec_from_config(&cfg, base, vec!["echo".into(), "hi".into()], &ctx);

    match prev_cage_env {
        Some(v) => std::env::set_var("ISOL8_CAGE", v),
        None => std::env::remove_var("ISOL8_CAGE"),
    }

    let spec = result.expect("spec_from_config");

    // Pre-set fields survive both the cage overlay and the config fill.
    assert_eq!(spec.profiles, vec!["cli-profile".to_string()]);
    assert_eq!(spec.add_dirs_rw, vec!["/cli-rw".to_string()]);

    // Empty field filled by the cage — which also wins over the config's own `home`.
    assert_eq!(spec.home.as_deref(), Some("/cage-home"));

    // Empty field the cage does not touch (no `ro` dirs) falls through to the config.
    assert_eq!(spec.add_dirs_ro, vec!["/cfg-ro".to_string()]);

    assert_eq!(spec.cmd, vec!["echo".to_string(), "hi".to_string()]);

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// cage::apply_overlay / select_name
// ---------------------------------------------------------------------------

#[test]
fn cage_apply_overlay_fills_only_empty_fields() {
    let tc = ToolchainChoice::new("nvm", "link").unwrap();
    let overlay = CageOverlay {
        profiles: vec!["cage-a".into()],
        home: Some("/cage-home".into()),
        ephemeral_home: false,
        add_dirs_rw: vec!["/cage-rw".into()],
        add_dirs_ro: vec!["/cage-ro".into()],
        toolchains: vec![tc.clone()],
        name: "x".into(),
        source: PathBuf::from("/x.toml"),
    };

    let mut spec = Spec::default();
    spec.profiles = vec!["preset-profile".into()]; // pre-set — must survive
    spec.add_dirs_rw = vec!["/preset-rw".into()]; // pre-set — must survive

    cage::apply_overlay(&overlay, &mut spec);

    assert_eq!(spec.profiles, vec!["preset-profile".to_string()]);
    assert_eq!(spec.add_dirs_rw, vec!["/preset-rw".to_string()]);
    // Empty fields get filled from the overlay.
    assert_eq!(spec.home.as_deref(), Some("/cage-home"));
    assert_eq!(spec.add_dirs_ro, vec!["/cage-ro".to_string()]);
    assert_eq!(spec.toolchains, vec![tc]);
}

#[test]
fn cage_select_name_precedence() {
    let _g = ENV_LOCK.lock().unwrap();
    let prev = std::env::var_os("ISOL8_CAGE");
    std::env::remove_var("ISOL8_CAGE");

    let cfg_with_cage = Config {
        cage: Some("from-cfg".into()),
        ..Config::builtin_defaults()
    };
    let cfg_no_cage = Config::builtin_defaults();

    assert_eq!(
        cage::select_name(Some("explicit"), &cfg_with_cage).as_deref(),
        Some("explicit")
    );
    assert_eq!(
        cage::select_name(None, &cfg_with_cage).as_deref(),
        Some("from-cfg")
    );

    std::env::set_var("ISOL8_CAGE", "from-env");
    // ISOL8_CAGE beats cfg.cage...
    assert_eq!(
        cage::select_name(None, &cfg_with_cage).as_deref(),
        Some("from-env")
    );
    // ...but loses to an explicit flag.
    assert_eq!(
        cage::select_name(Some("explicit"), &cfg_with_cage).as_deref(),
        Some("explicit")
    );
    // With no cfg.cage at all, the env var still wins over "no selection".
    assert_eq!(
        cage::select_name(None, &cfg_no_cage).as_deref(),
        Some("from-env")
    );

    match prev {
        Some(v) => std::env::set_var("ISOL8_CAGE", v),
        None => std::env::remove_var("ISOL8_CAGE"),
    }
}

// ---------------------------------------------------------------------------
// resolve::effective_policy_in / sandbox::dry_run_in — Context is threaded,
// not re-read from the ambient environment.
// ---------------------------------------------------------------------------

#[test]
fn effective_policy_in_and_dry_run_in_thread_context_home() {
    let root = tmp_dir("ctx-threading");
    let real_a = root.join("home-a");
    let real_b = root.join("home-b");
    std::fs::create_dir_all(&real_a).unwrap();
    std::fs::create_dir_all(&real_b).unwrap();
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let config_dir = root.join("cfg");
    std::fs::create_dir_all(&config_dir).unwrap();

    let ctx_a = Context {
        real_home: real_a.clone(),
        cwd: cwd.clone(),
        platform: Platform::current(),
        config_dir: config_dir.clone(),
        managed_root: config_dir.join("homes"),
    };
    let ctx_b = Context {
        real_home: real_b.clone(),
        cwd,
        platform: Platform::current(),
        config_dir: config_dir.clone(),
        managed_root: config_dir.join("homes"),
    };

    let mut spec = Spec::default();
    spec.profiles = vec!["base".into()];
    spec.cmd = vec!["echo".into(), "hi".into()];
    spec.add_dirs_rw = vec!["~/isol8-ctx-marker".into()];

    let expected_a = real_a.join("isol8-ctx-marker").display().to_string();
    let expected_b = real_b.join("isol8-ctx-marker").display().to_string();
    assert_ne!(expected_a, expected_b);

    let eff_a = resolve::effective_policy_in(&spec, &ctx_a).expect("effective_policy_in a");
    let eff_b = resolve::effective_policy_in(&spec, &ctx_b).expect("effective_policy_in b");

    assert!(
        eff_a.profile.paths.iter().any(|g| g.path == expected_a),
        "{:?}",
        eff_a.profile.paths
    );
    assert!(
        eff_b.profile.paths.iter().any(|g| g.path == expected_b),
        "{:?}",
        eff_b.profile.paths
    );

    let dry_a = sandbox::dry_run_in(&spec, &ctx_a).expect("dry_run_in a");
    let dry_b = sandbox::dry_run_in(&spec, &ctx_b).expect("dry_run_in b");

    assert!(dry_a.profile.paths.iter().any(|g| g.path == expected_a));
    assert!(dry_b.profile.paths.iter().any(|g| g.path == expected_b));

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// serde: DryRun structure + stable wire-contract enum spellings for `--json`.
// ---------------------------------------------------------------------------

#[test]
fn dry_run_json_structure_and_enum_wire_spellings() {
    let root = tmp_dir("dry-run-json");
    let real_home = root.join("home");
    std::fs::create_dir_all(&real_home).unwrap();
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let config_dir = root.join("cfg");
    std::fs::create_dir_all(&config_dir).unwrap();

    let ctx = Context {
        real_home,
        cwd,
        platform: Platform::current(),
        config_dir: config_dir.clone(),
        managed_root: config_dir.join("homes"),
    };

    let mut spec = Spec::default();
    spec.profiles = vec!["base".into()];
    spec.cmd = vec!["echo".into(), "hi".into()];

    let dry = sandbox::dry_run_in(&spec, &ctx).expect("dry_run_in");
    let json = serde_json::to_value(&dry).expect("to_value");

    assert!(json["layer_names"].is_array(), "{json}");
    assert!(json["profile"]["paths"].is_array(), "{json}");
    assert!(!json["home_plan"].is_null(), "{json}");

    // These string forms are a wire contract for `--json` consumers — pin them.
    assert_eq!(
        serde_json::to_value(PlanAction::Apply).unwrap(),
        serde_json::json!("apply")
    );
    assert_eq!(
        serde_json::to_value(PlanAction::SkipExists).unwrap(),
        serde_json::json!("skip-exists")
    );
    assert_eq!(
        serde_json::to_value(PlanAction::SkipMissingSource).unwrap(),
        serde_json::json!("skip-missing")
    );
    assert_eq!(
        serde_json::to_value(HomeOpKind::SeedRo).unwrap(),
        serde_json::json!("seed-ro")
    );
    assert_eq!(
        serde_json::to_value(LayerOrigin::Explicit).unwrap(),
        serde_json::json!("explicit")
    );
    assert_eq!(
        serde_json::to_value(StrategyName::Link).unwrap(),
        serde_json::json!("link")
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// analyze::run_and_analyze — deterministic via the NDJSON feed, no platform
// observer required.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn run_and_analyze_via_ndjson_feed_is_deterministic() {
    let _g = ENV_LOCK.lock().unwrap();
    let prev_feed = std::env::var_os("ISOL8_ANALYZE_FEED");

    let root = tmp_dir("analyze-ndjson");
    let feed = root.join("feed.ndjson");
    std::fs::write(
        &feed,
        "{\"path\":\"/Users/alice/.m2/repository/org/foo/1.0/foo.jar\",\"access\":\"read\",\"count\":5}\n",
    )
    .unwrap();
    std::env::set_var("ISOL8_ANALYZE_FEED", &feed);

    let real_home = root.join("home");
    std::fs::create_dir_all(&real_home).unwrap();
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let config_dir = root.join("cfg");
    std::fs::create_dir_all(&config_dir).unwrap();
    let ctx = Context {
        real_home,
        cwd,
        platform: Platform::current(),
        config_dir: config_dir.clone(),
        managed_root: config_dir.join("homes"),
    };

    let mut spec = Spec::default();
    spec.profiles = Config::builtin_defaults().default_profiles;
    spec.cmd = vec!["/bin/echo".into(), "hi".into()];

    let result = analyze::run_and_analyze(&spec, &ctx);

    match prev_feed {
        Some(v) => std::env::set_var("ISOL8_ANALYZE_FEED", v),
        None => std::env::remove_var("ISOL8_ANALYZE_FEED"),
    }

    let outcome = result.expect("run_and_analyze");
    assert_eq!(outcome.code, 0, "outcome: {outcome:?}");
    assert!(
        outcome.report.source_note.contains("NDJSON"),
        "{}",
        outcome.report.source_note
    );

    let _ = std::fs::remove_dir_all(&root);
}
