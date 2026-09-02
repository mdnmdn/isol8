use std::collections::HashMap;
use std::process::Output;

use crate::error::Result;
use crate::profile::Profile;
#[cfg(unix)]
use crate::pty::SandboxStdio;
use crate::sandbox::SandboxChild;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

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

    /// Apply the policy and launch `cmd` with the child's standard streams wired
    /// to `stdio`, returning a non-blocking handle.
    ///
    /// This is the pseudo-terminal seam (unix only): a host that owns a pty hands
    /// over the slave via [`SandboxStdio::from_tty`], and the confined process gets
    /// a controlling terminal established **before** the policy is applied, so the
    /// `TIOCSCTTY` ioctl is not itself denied. There is no supervisor shim — macOS
    /// `sandbox-exec` execs in place and Linux forks exactly once, so the returned
    /// handle is the harness's own pid.
    ///
    /// `Backend` is closed to external implementation ([`SandboxChild`]'s
    /// constructors are `pub(crate)`), so this method has no default body.
    #[cfg(unix)]
    fn spawn_with_stdio(
        &self,
        profile: &Profile,
        env: &HashMap<String, String>,
        cmd: &[String],
        stdio: SandboxStdio,
    ) -> Result<SandboxChild>;

    /// Apply the policy, run `cmd` to completion, and capture stdout/stderr.
    ///
    /// Used by `@cage verify`. Default falls back to spawn+wait without body
    /// capture (stdout/stderr empty); backends should override when possible.
    fn output(
        &self,
        profile: &Profile,
        env: &HashMap<String, String>,
        cmd: &[String],
    ) -> Result<Output> {
        let mut child = self.spawn(profile, env, cmd)?;
        let code = child.wait()?;
        Ok(Output {
            status: exit_status_from_code(code),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    /// Render the merged profile into the OS-native policy text (Seatbelt SBPL,
    /// Landlock rules, …) for dry-run / introspection — no side effects.
    fn render_policy(&self, profile: &Profile) -> String;
}

fn exit_status_from_code(code: i32) -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        // Windows exit codes are u32; negative is unusual.
        std::process::ExitStatus::from_raw(code as u32)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = code;
        unreachable!("no exit status conversion on this platform")
    }
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
