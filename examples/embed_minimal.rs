//! Smallest working confinement — engine only.
//!
//! Doubles as the compile gate for `default-features = false`: if this stops
//! building without the `cli` / `registry` features, the engine-only embed that
//! README and `_docs/embedding.md` advertise is broken.
//!
//! ```sh
//! cargo run --no-default-features --example embed_minimal
//! ```

fn main() -> isol8::Result<()> {
    let project = std::env::current_dir()?;

    // Deny-by-default: only `base` (+ the OS system-runtime layer it requires)
    // and this one read-write grant.
    let code = isol8::Sandbox::new()
        .profile("base")
        .profile(system_runtime())
        .grant_rw(project.display().to_string())
        .run(["echo", "hello from inside the sandbox"])?;

    println!("exit code: {code}");
    Ok(())
}

/// The per-OS runtime layer (dynamic loader, libc, …) a real command needs.
fn system_runtime() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos/system-runtime"
    } else if cfg!(target_os = "linux") {
        "linux/system-runtime"
    } else {
        "windows/system-runtime"
    }
}
