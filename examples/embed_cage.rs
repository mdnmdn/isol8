//! Cage discovery → overlay → confined run.
//!
//! A cage is a *selection* layer: a named bundle of profiles, home mode, dirs and
//! toolchain choices. It is not a profile and does not take part in the
//! deny-first merge — it only fills `Spec` fields the caller left empty.
//!
//! ```sh
//! cargo run --example embed_cage            # default cage discovery
//! cargo run --example embed_cage -- work    # a named cage
//! ```

fn main() -> isol8::Result<()> {
    isol8::ensure_registry_provider();

    let requested = std::env::args().nth(1);
    let ctx = isol8::Context::from_environment()?;
    let cfg = isol8::config::load()?;

    println!("== available cages ==");
    for (name, path) in isol8::cage::list_cages_in(&ctx.cwd, Some(&ctx.config_dir))? {
        println!("  {name:<16} {}", path.display());
    }

    // Name precedence: explicit argument → ISOL8_CAGE → config `cage`.
    // `None` still runs default discovery (.isol8/cage.toml, cages/default.toml).
    let name = isol8::cage::select_name(requested.as_deref(), &cfg);
    let Some(cage) = isol8::cage::resolve_in(name.as_deref(), &ctx.cwd, Some(&ctx.config_dir))?
    else {
        println!("\nno cage found — nothing to do");
        return Ok(());
    };

    println!("\n== resolved cage ==");
    print!("{}", isol8::cage::format_show(&cage));

    // Fields already set on the Spec are never overwritten by the overlay.
    let mut spec = isol8::Spec::new(["echo", "hi"]);
    isol8::cage::apply_overlay(&cage.overlay(), &mut spec);
    if spec.profiles.is_empty() {
        spec.profiles = cfg.default_profiles.clone();
    }

    let dry = isol8::sandbox::dry_run(&spec)?;
    println!("\n== effective ==");
    println!("  home   : {}", dry.home_path.display());
    println!("  layers : {}", dry.layer_names.len());
    println!("  grants : {}", dry.profile.paths.len());
    for (id, strategy) in &dry.recipes {
        println!("  recipe : {id} ({strategy})");
    }

    Ok(())
}
