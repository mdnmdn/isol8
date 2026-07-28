//! Reproduce the CLI's own resolution headlessly: config file + project marker +
//! `ISOL8_*` overrides + cage → `Spec` → dry-run.
//!
//! The output should match `isol8 --show-policies -- echo hi` for the same
//! directory and config. That equivalence is the point of `spec_from_config`:
//! an embedder gets isol8's documented behaviour without reimplementing
//! `_docs/config.md`.
//!
//! ```sh
//! cargo run --example embed_config
//! ISOL8_CONFIG_PATH=./_data/config cargo run --example embed_config
//! ```

fn main() -> isol8::Result<()> {
    // Offline registry recipe dirs from `[registries.*]`. Process-global,
    // first call wins; harmless if you have none configured.
    isol8::ensure_registry_provider();

    // 1. Discovery: ISOL8_CONFIG_PATH → project marker (isol8.toml / .isol8.toml
    //    / encage.toml) → ~/.config/isol8/isol8.toml → builtin defaults.
    let mut cfg = isol8::config::load()?;

    // 2. ISOL8_PROFILE / ISOL8_HOME / ISOL8_ADD_DIRS_* … override the file.
    isol8::config::apply_env_overrides(&mut cfg);

    println!(
        "config dir     : {}",
        isol8::config::effective_config_dir().display()
    );
    println!("default profiles: {:?}", cfg.default_profiles);
    println!("auto profiles  : {}", cfg.auto_profiles);
    if let Some(cage) = &cfg.cage {
        println!("cage           : {cage}");
    }

    // 3. Config → Spec. Anything pre-set on the base Spec models a CLI flag and
    //    wins over the config; the cage then fills whatever is still empty.
    let ctx = isol8::Context::from_environment()?;
    let spec = isol8::resolve::spec_from_config(
        &cfg,
        isol8::Spec::default(),
        vec!["echo".into(), "hi".into()],
        &ctx,
    )?;

    // 4. Resolve without spawning and without touching the filesystem.
    let dry = isol8::sandbox::dry_run(&spec)?;

    println!("\n== layers ==");
    for (name, origin) in &dry.layer_names {
        println!("  {name} ({})", origin.label());
    }

    println!("\n== grants ==");
    for g in &dry.profile.paths {
        println!("  {:?} {:?} {}", g.access, g.r#match, g.path);
    }

    println!("\n== home ==");
    println!("  {}", dry.home_path.display());
    let plan = dry.home_plan.render();
    if !plan.trim().is_empty() {
        print!("{plan}");
    }

    Ok(())
}
