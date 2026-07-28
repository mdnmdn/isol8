//! Run a command and turn its denials into recipe suggestions.
//!
//! Denial observation is platform-dependent and always best-effort: macOS reads
//! the unified log, Linux has no denial log at all (Landlock is silent), and any
//! platform can be fed NDJSON via `ISOL8_ANALYZE_FEED`. The report says which
//! source it used, and never claims to be exhaustive.
//!
//! ```sh
//! cargo run --example embed_analyze
//! ISOL8_ANALYZE_FEED=denials.ndjson cargo run --example embed_analyze
//! ```

fn main() -> isol8::Result<()> {
    isol8::ensure_registry_provider();

    // Deliberately under-grant: read a path no layer allows, so there is
    // something to observe.
    let mut spec = isol8::Spec::new(["/bin/cat", "/etc/hosts"]);
    spec.profiles = vec!["base".into()];

    let ctx = isol8::Context::from_environment()?;
    let outcome = isol8::analyze::run_and_analyze(&spec, &ctx)?;

    println!("exit code : {}", outcome.code);
    println!("pid       : {}", outcome.pid);
    println!("denials   : {}", outcome.report.total_denials);
    println!("source    : {}", outcome.report.source_note);
    println!();
    print!("{}", outcome.report.render());

    // The structured form is what you would act on programmatically.
    for item in &outcome.report.items {
        match (&item.recipe_id, &item.strategy) {
            (Some(id), Some(s)) => println!("suggest: {id} strategy={}", s.as_str()),
            _ if item.needs_home_link => {
                println!("missing materialization: {}", item.root.display())
            }
            _ => println!(
                "missing grant: {} ({})",
                item.root.display(),
                item.access.short()
            ),
        }
    }

    Ok(())
}
