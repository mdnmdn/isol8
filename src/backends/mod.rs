use std::collections::HashMap;

use crate::error::Result;
use crate::profile::Profile;
use crate::sandbox::SandboxChild;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(windows)]
pub(crate) mod windows;
#[cfg(windows)]
mod windows_hook;
#[cfg(windows)]
mod windows_policy;

/// Whether path grants are enforced on Windows (requires `isol8-winhook.dll` beside the binary).
#[cfg(windows)]
pub fn path_enforcement_available() -> bool {
    windows_hook::hook_dll_available().is_some()
}

/// On non-Windows platforms path enforcement is handled by Landlock/Seatbelt, not this function.
#[cfg(not(windows))]
pub fn path_enforcement_available() -> bool {
    false
}

/// A platform sandbox implementation. Renders the merged `Profile` into the
/// OS-native policy (Landlock ruleset, Seatbelt text, …) and execs the command.
pub trait Backend {
    /// Apply the policy and launch `cmd`, returning a non-blocking handle.
    ///
    /// The child is *not* waited on; call [`SandboxChild::wait`] to block and
    /// collect the exit code (which the handle interprets per backend).
    fn spawn(
        &self,
        profile: &Profile,
        env: &HashMap<String, String>,
        cmd: &[String],
    ) -> Result<SandboxChild>;

    /// Render the merged profile into the OS-native policy text (Seatbelt SBPL,
    /// Landlock rules, …) for dry-run / introspection — no side effects.
    fn render_policy(&self, profile: &Profile) -> String;
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
    #[cfg(windows)]
    {
        Box::new(windows::WindowsBackend)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        compile_error!("no sandbox backend for this OS")
    }
}
