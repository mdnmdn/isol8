//! Serialize a resolved policy — what a non-Rust host parses.
//!
//! Every report type derives `Serialize`, so a host can either link this crate or
//! spawn `isol8 --show-policies --json` and parse the identical shape.
//!
//! ```sh
//! cargo run --no-default-features --example embed_json | jq '.profile.paths'
//! ```

fn main() -> isol8::Result<()> {
    let mut spec = isol8::Spec::new(["echo", "hi"]);
    spec.profiles = vec!["base".into(), system_runtime().into()];

    let dry = isol8::sandbox::dry_run(&spec)?;

    let json = serde_json::to_string_pretty(&dry)
        .map_err(|e| isol8::Error::Message(format!("serializing DryRun: {e}")))?;
    println!("{json}");

    Ok(())
}

fn system_runtime() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos/system-runtime"
    } else if cfg!(target_os = "linux") {
        "linux/system-runtime"
    } else {
        "windows/system-runtime"
    }
}
