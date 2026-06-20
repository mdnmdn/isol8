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
    fn spawn(&self, profile: &Profile, env: &HashMap<String, String>, cmd: &[String]) -> Result<i32>;
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

/// Print the effective policy for `--dry-run`.
///
/// ponytail: stub. Should render the merged grants, sanitized env, and target
/// command in a human-readable form — first-class per spec (developer/agent trust).
pub fn render_dry_run(_profile: &Profile, _env: &HashMap<String, String>, _cmd: &[String]) {
    todo!("render effective policy: grants, env, command")
}
