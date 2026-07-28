//! Author a cage from a host program — no clap, no prompts.
//!
//! The interactive wizard lives in the CLI; these are the same steps it drives,
//! available under the `wizard` feature so a host can build its own UI over them.
//!
//! Plan/apply throughout: `render` shows exactly what `apply` would write, and
//! drift protection refuses to clobber a hand-edited cage.
//!
//! ```sh
//! cargo run --no-default-features --features wizard --example embed_wizard
//! ```

use isol8::profile::Access;
use isol8::wizard::{self, DriftStatus, WizardRequest};

fn main() -> isol8::Result<()> {
    isol8::ensure_registry_provider();

    let reg = isol8::RecipeRegistry::load(&[])?;
    let rc = isol8::filter::RunContext::from_cmd(&[]);
    let real = isol8::home::real_home();

    // Offer only what this machine actually has, at each recipe's default strategy.
    let found: Vec<String> = isol8::detect::detect_all(&reg, &rc, &real)?
        .into_iter()
        .filter(|r| r.found)
        .map(|r| r.id)
        .collect();
    let tools = wizard::tools_from_detect(&reg, &found)?;

    let req = WizardRequest {
        name: "embed-example".into(),
        home: "managed".into(), // → home = "@managed/embed-example"
        tools,
        dirs: vec![(std::env::current_dir()?.display().to_string(), Access::Rw)],
        profiles: Vec::new(), // empty → config default_profiles apply at run time
        out_dir: Some(std::env::temp_dir().join("isol8-embed-example")),
        force: false,
        existing_path: None,
    };

    // Flag strategies that would grant rw on the real home, before writing.
    for note in wizard::preview_security_notes(&req.tools, &reg) {
        eprintln!("security: {note}");
    }

    // Preview — nothing written.
    let preview = wizard::render(&req)?;
    println!("== would write {} ==", preview.path.display());
    print!("{}", preview.body);
    for w in &preview.warnings {
        eprintln!("warning: {w}");
    }

    // Refuse to overwrite a cage someone hand-edited since the last wizard write.
    let state_file = wizard::state_path();
    let state = wizard::load_state(&state_file)?;
    match wizard::check_drift(&req.name, &preview.path, &state)? {
        DriftStatus::Drift {
            expected, actual, ..
        } => {
            eprintln!("hand-edited since last write (expected {expected}, found {actual})");
            eprintln!("re-run with force: true to overwrite");
            return Ok(());
        }
        status => println!("\ndrift check: {status:?}"),
    }

    if std::env::var("ISOL8_EXAMPLE_APPLY").is_ok() {
        let result = wizard::apply(&req, &state_file)?;
        println!("wrote {}", result.path.display());
    } else {
        println!("(set ISOL8_EXAMPLE_APPLY=1 to actually write it)");
    }

    Ok(())
}
