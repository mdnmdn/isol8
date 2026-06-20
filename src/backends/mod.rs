use std::collections::HashMap;

use anyhow::Result;

use crate::profile::Profile;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// A platform sandbox implementation. Renders the merged `Profile` into the
/// OS-native policy (Landlock ruleset, Seatbelt text, …) and execs the command.
pub trait Backend {
    /// Apply the policy and run `cmd`, returning its exit code.
    fn spawn(
        &self,
        profile: &Profile,
        env: &HashMap<String, String>,
        cmd: &[String],
    ) -> Result<i32>;
}

/// Select the backend for the current OS.
pub fn select() -> Box<dyn Backend> {
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxBackend)
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacosBackend)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        compile_error!("no sandbox backend for this OS yet (Windows is deferred)")
    }
}

/// Print the effective policy for `--dry-run`: the merged path grants, the sorted
/// sanitized env, the resolved HOME, the target command, and (on macOS) the
/// generated SBPL. Plain text — structured/JSON output is Phase 4.
pub fn render_dry_run(profile: &Profile, env: &HashMap<String, String>, cmd: &[String]) {
    println!("== isol8 effective policy (dry-run) ==");

    println!("\n-- path grants --");
    if profile.paths.is_empty() {
        println!("  (none — deny-by-default; nothing is reachable)");
    } else {
        for g in &profile.paths {
            println!(
                "  {:<8} {:<8} {}",
                format!("{:?}", g.access).to_lowercase(),
                format!("{:?}", g.r#match).to_lowercase(),
                g.path
            );
        }
    }

    println!("\n-- environment --");
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    let home = env.get("HOME").map(String::as_str).unwrap_or("(unset)");
    println!("  HOME = {home}");
    if keys.is_empty() {
        println!("  (empty)");
    } else {
        for k in keys {
            if k == "HOME" {
                continue; // already printed first
            }
            println!("  {k} = {}", env[k]);
        }
    }

    println!("\n-- command --");
    if cmd.is_empty() {
        println!("  (none)");
    } else {
        println!("  {}", cmd.join(" "));
    }

    #[cfg(target_os = "macos")]
    {
        println!("\n-- generated Seatbelt policy (SBPL) --");
        print!("{}", macos::render_policy(profile));
    }
}
