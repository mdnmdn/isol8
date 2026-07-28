//! Enumerate recipes, detect installed toolchains, and materialize a home.
//!
//! Shows the three things a host integrating isol8 usually needs: *what can I
//! offer the user*, *what does this machine actually have*, and *prepare the
//! isolated home without surprising them*.
//!
//! ```sh
//! cargo run --example embed_recipes
//! ```

fn main() -> isol8::Result<()> {
    isol8::ensure_registry_provider();

    // Builtins + ~/.config/isol8/recipes + any offline registry dirs. Build once
    // and reuse — this is the cache.
    let reg = isol8::RecipeRegistry::load(&[])?;
    let rc = isol8::filter::RunContext::from_cmd(&[]);

    println!("== available recipes ==");
    for id in reg.ids() {
        // Variants are platform-filtered; a recipe with no match here is simply
        // not offered on this host.
        match reg.resolve(&id, &rc) {
            Ok(r) => println!("  {id:<24} {}", r.summary),
            Err(_) => println!("  {id:<24} (not available on this platform)"),
        }
    }

    println!("\n== detected on this host ==");
    let real = isol8::home::real_home();
    let rows = isol8::detect::detect_all(&reg, &rc, &real)?;
    print!("{}", isol8::detect::format_detect_table(&rows));

    // Compile the first detected toolchain into grants + home ops + env.
    let Some(found) = rows.iter().find(|r| r.found) else {
        println!("\nnothing detected — skipping materialization");
        return Ok(());
    };
    let recipe = reg.resolve(&found.id, &rc)?;
    // `default_strategy` when the recipe declares one, else whichever it defines.
    let strategy = recipe
        .default_strategy
        .or_else(|| recipe.strategies.keys().next().copied())
        .unwrap_or(isol8::StrategyName::Link);
    let choice = isol8::ToolchainChoice::new(&found.id, strategy.as_str())?;
    let contribution = reg.compile(&choice, &rc)?;

    println!("\n== {} / {} ==", found.id, strategy.as_str());
    for g in &contribution.paths {
        println!("  grant {:?} {}", g.access, g.path);
    }
    for (k, v) in &contribution.env {
        println!("  env   {k}={v}");
    }

    // Plan before apply: compute the mutations, show them, then write.
    let ctx = isol8::Context::from_environment()?;
    let home = ctx.managed_home("embed-example")?;
    let plan = isol8::HomePlan::compute(&contribution.home_ops, &ctx, &home)?;

    println!("\n== home plan for {} ==", home.display());
    print!("{}", plan.render());

    if std::env::var("ISOL8_EXAMPLE_APPLY").is_ok() {
        plan.apply()?; // idempotent
        println!("applied {} op(s)", plan.apply_count());
    } else {
        println!("(set ISOL8_EXAMPLE_APPLY=1 to actually create it)");
    }

    Ok(())
}
