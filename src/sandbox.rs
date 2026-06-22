//! Engine entry surface: the clap-free [`Spec`] consumed by the resolve pipeline,
//! the non-blocking [`SandboxChild`] handle, and (added in later steps) the
//! [`Sandbox`] builder and structured dry-run.

use std::collections::HashMap;

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
};

#[cfg(target_os = "macos")]
use crate::error::ResultExt;
use crate::error::{Error, Result};
use crate::profile::Profile;

/// A clap-free description of a confinement request.
///
/// Mirrors the CLI `ProfileOpts` plus the command to run. The engine pipeline
/// (`resolve::effective_policy`) reads this directly, so an embedder never has to
/// construct a clap-derived type. Build one by hand, via [`crate::cli::ProfileOpts::into_spec`],
/// or (ergonomically) through the [`Sandbox`] builder.
#[derive(Clone, Default, Debug)]
pub struct Spec {
    /// Named profile layers to enable (deny-first merge order).
    pub profiles: Vec<String>,
    /// Extra profile directories / TOML files (override same-named built-ins).
    pub profile_paths: Vec<String>,
    /// Auto-select layers whose executable filter matches the command.
    pub auto_profiles: bool,
    /// Extra read-write path grants.
    pub add_dirs_rw: Vec<String>,
    /// Extra read-only path grants.
    pub add_dirs_ro: Vec<String>,
    /// Grant the auto-added cwd read-only instead of read-write.
    pub cwd_ro: bool,
    /// Replacement `$HOME` (overrides any profile `home_replace`).
    pub home: Option<String>,
    /// Skip seeding real-home files into the (replacement) home.
    pub no_seed: bool,
    /// Host env vars to pass through by name (highest precedence after `set_env`).
    pub env_pass: Vec<String>,
    /// Explicit `K=V` env entries (highest precedence).
    pub set_env: Vec<String>,
    /// The command (and arguments) to confine.
    pub cmd: Vec<String>,
}

/// A handle to a launched, confined process.
///
/// [`Backend::spawn`](crate::backends::Backend::spawn) returns this **without**
/// waiting, so an embedder can hold the child, read its [`id`](SandboxChild::id),
/// [`kill`](SandboxChild::kill) it, or block on [`wait`](SandboxChild::wait).
///
/// The backends are heterogeneous: macOS launches a `sandbox-exec`
/// `std::process::Child`; Linux forks and keeps the raw `Pid`; Windows uses a
/// raw `HANDLE` from `CreateProcessW` under an AppContainer. The `on_exit`
/// closure maps a raw exit code into a rich error where the OS overloads exit codes
/// for its own failures (macOS `sandbox-exec` 64/65/71/134); elsewhere it is the
/// identity.
pub struct SandboxChild {
    handle: Handle,
    on_exit: Box<dyn Fn(i32) -> Result<i32>>,
}

enum Handle {
    /// macOS: the launched `sandbox-exec` child.
    #[cfg(target_os = "macos")]
    Process(std::process::Child),
    /// Linux: a forked child set up + exec'd in the fork; reaped via `waitpid`.
    #[cfg(target_os = "linux")]
    Forked(nix::unistd::Pid),
    /// Windows: live handle from CreateProcessW under AppContainer.
    #[cfg(windows)]
    Windows {
        pid: u32,
        h_process: HANDLE,
        /// AppContainer name (if created via CreateAppContainerProfile) for best-effort
        /// DeleteAppContainerProfile on wait.
        container_name: Option<String>,
    },
    /// A process whose exit code is already known (legacy or immediate-fail path).
    #[allow(dead_code)]
    Exited(i32),
}

impl SandboxChild {
    /// macOS: wrap a launched child plus its exit-code interpreter.
    #[cfg(target_os = "macos")]
    pub(crate) fn process(
        child: std::process::Child,
        on_exit: Box<dyn Fn(i32) -> Result<i32>>,
    ) -> Self {
        Self {
            handle: Handle::Process(child),
            on_exit,
        }
    }

    /// Linux: wrap a forked child reaped via `waitpid` (identity exit mapping).
    #[cfg(target_os = "linux")]
    pub(crate) fn forked(pid: nix::unistd::Pid) -> Self {
        Self {
            handle: Handle::Forked(pid),
            on_exit: Box::new(Ok),
        }
    }

    /// A process that already finished with `code` (identity exit mapping).
    #[allow(dead_code)]
    pub(crate) fn exited(code: i32) -> Self {
        Self {
            handle: Handle::Exited(code),
            on_exit: Box::new(Ok),
        }
    }

    /// Windows: wrap a live AppContainer-launched process + optional container name for cleanup.
    #[cfg(windows)]
    pub(crate) fn windows(pid: u32, h_process: HANDLE, container_name: Option<String>) -> Self {
        Self {
            handle: Handle::Windows {
                pid,
                h_process,
                container_name,
            },
            on_exit: Box::new(Ok),
        }
    }

    /// The child's process id (`0` for an already-finished handle).
    pub fn id(&self) -> u32 {
        match &self.handle {
            #[cfg(target_os = "macos")]
            Handle::Process(c) => c.id(),
            #[cfg(target_os = "linux")]
            Handle::Forked(p) => p.as_raw() as u32,
            #[cfg(windows)]
            Handle::Windows { pid, .. } => *pid,
            Handle::Exited(_) => 0,
        }
    }

    /// Block until the child exits, returning its exit code (after backend-specific
    /// interpretation). A backend that overloads exit codes for its own failures
    /// surfaces those as a rich [`Error`] here.
    pub fn wait(&mut self) -> Result<i32> {
        let code = match &mut self.handle {
            #[cfg(target_os = "macos")]
            Handle::Process(c) => {
                let status = c.wait().ctx(|| "waiting for the sandboxed child")?;
                exit_code(&status)
            }
            #[cfg(target_os = "linux")]
            Handle::Forked(pid) => {
                let status = nix::sys::wait::waitpid(*pid, None)
                    .map_err(|e| Error::Message(format!("waitpid failed: {e}")))?;
                wait_status_code(&status)
            }
            #[cfg(windows)]
            Handle::Windows {
                h_process,
                container_name,
                ..
            } => {
                let mut code: i32 = 0;
                unsafe {
                    if !h_process.0.is_null() {
                        WaitForSingleObject(*h_process, 0xFFFFFFFF);
                        let mut exit_code: u32 = 0;
                        let _ = GetExitCodeProcess(*h_process, &mut exit_code as *mut u32);
                        code = exit_code as i32;
                        let _ = CloseHandle(*h_process);
                        h_process.0 = std::ptr::null_mut();
                        // best-effort cleanup of named AppContainer profile
                        if let Some(name) = container_name.take() {
                            let _ = delete_app_container_profile_by_name(&name);
                        }
                    }
                }
                code
            }
            Handle::Exited(code) => *code,
        };
        (self.on_exit)(code)
    }

    /// Forcibly terminate the child. A no-op for an already-finished handle.
    pub fn kill(&mut self) -> Result<()> {
        match &mut self.handle {
            #[cfg(target_os = "macos")]
            Handle::Process(c) => c.kill().map_err(Error::from),
            #[cfg(target_os = "linux")]
            Handle::Forked(pid) => nix::sys::signal::kill(*pid, nix::sys::signal::Signal::SIGKILL)
                .map_err(|e| Error::Message(format!("kill failed: {e}"))),
            #[cfg(windows)]
            Handle::Windows {
                h_process,
                container_name,
                ..
            } => {
                unsafe {
                    if !h_process.0.is_null() {
                        let _ = TerminateProcess(*h_process, 1);
                        let _ = CloseHandle(*h_process);
                        h_process.0 = std::ptr::null_mut();
                        if let Some(name) = container_name.take() {
                            let _ = delete_app_container_profile_by_name(&name);
                        }
                    }
                }
                Ok(())
            }
            Handle::Exited(_) => Ok(()),
        }
    }
}

/// Map a child `ExitStatus` to a shell-style exit code: the real code, or 128+signo
/// if signal-terminated (unix), else 1.
#[cfg(target_os = "macos")]
pub(crate) fn exit_code(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// Map a Linux `WaitStatus` to a shell-style exit code.
#[cfg(target_os = "linux")]
fn wait_status_code(status: &nix::sys::wait::WaitStatus) -> i32 {
    match status {
        nix::sys::wait::WaitStatus::Exited(_, code) => *code,
        nix::sys::wait::WaitStatus::Signaled(_, sig, _) => 128 + (*sig as i32),
        _ => 1,
    }
}

/// Best-effort delete of a named AppContainer profile. Defined here so SandboxChild
/// wait/kill can call it under cfg(windows) without leaking backend internals.
/// The actual call is forwarded; on non-windows this is a no-op stub.
#[cfg(windows)]
fn delete_app_container_profile_by_name(name: &str) -> crate::error::Result<()> {
    // Delegate to backend helper (defined in backends/windows.rs)
    crate::backends::windows::delete_app_container_profile(name)
}

#[cfg(not(windows))]
#[allow(dead_code)]
fn delete_app_container_profile_by_name(_name: &str) -> crate::error::Result<()> {
    Ok(())
}

/// A structured, side-effect-free dry run: the resolved layer stack (with
/// provenance), the merged profile, the sanitized env, the (rewritten) command, and
/// the rendered OS-native policy text. The CLI turns this into the `--show-policies`
/// report; an embedder inspects the fields directly.
pub struct DryRun {
    /// The resolved layer stack (deps-first) tagged with provenance.
    pub layer_names: Vec<(String, crate::resolve::LayerOrigin)>,
    /// The merged, deny-first profile.
    pub profile: Profile,
    /// The sanitized environment for the confined process.
    pub env: HashMap<String, String>,
    /// The command after profile `rewrite` rules are applied.
    pub cmd: Vec<String>,
    /// The rendered OS-native policy text (Seatbelt SBPL, Landlock rules, …).
    pub policy: String,
    /// A human label for `policy` (e.g. "generated Seatbelt policy (SBPL)").
    pub policy_label: &'static str,
}

/// Resolve the effective policy for `spec` and render the OS-native policy text,
/// without spawning. Pure data — no printing.
pub fn dry_run(spec: &Spec) -> Result<DryRun> {
    let eff = crate::resolve::effective_policy(spec)?;
    let policy = crate::backends::select().render_policy(&eff.profile);
    let policy_label = match std::env::consts::OS {
        "macos" => "generated Seatbelt policy (SBPL)",
        "linux" => "generated Landlock rules",
        "windows" => "generated AppContainer policy",
        _ => "generated policy",
    };
    Ok(DryRun {
        layer_names: eff.layer_names,
        profile: eff.profile,
        env: eff.env,
        cmd: eff.cmd,
        policy,
        policy_label,
    })
}

/// Guard against running isol8 inside an isol8 sandbox (Seatbelt cannot nest).
/// Returns [`Error::NestedSandbox`] when the [`crate::env::SANDBOX_MARKER`] is set.
pub fn ensure_not_nested() -> Result<()> {
    if std::env::var_os(crate::env::SANDBOX_MARKER).is_some() {
        return Err(Error::NestedSandbox);
    }
    Ok(())
}

/// Ergonomic builder over [`Spec`] for embedding isol8.
///
/// ```no_run
/// let code = isol8::Sandbox::new()
///     .profile("base")
///     .grant_rw("/my/project")
///     .run(["node", "script.js"])?;          // → exit code (blocking)
/// # Ok::<(), isol8::Error>(())
/// ```
#[derive(Clone, Default)]
pub struct Sandbox {
    spec: Spec,
}

impl Sandbox {
    /// A new builder with default (deny-by-default) settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start from an existing [`Spec`].
    pub fn from_spec(spec: Spec) -> Self {
        Self { spec }
    }

    /// Mutable access to the underlying [`Spec`] for fields without a builder method.
    pub fn spec_mut(&mut self) -> &mut Spec {
        &mut self.spec
    }

    /// Enable a named profile layer (repeatable).
    pub fn profile(mut self, name: impl Into<String>) -> Self {
        self.spec.profiles.push(name.into());
        self
    }

    /// Add an extra profile directory / TOML file (repeatable).
    pub fn profile_path(mut self, path: impl Into<String>) -> Self {
        self.spec.profile_paths.push(path.into());
        self
    }

    /// Auto-select layers whose executable filter matches the command.
    pub fn auto_profiles(mut self, on: bool) -> Self {
        self.spec.auto_profiles = on;
        self
    }

    /// Grant read-write access to a path (repeatable).
    pub fn grant_rw(mut self, path: impl Into<String>) -> Self {
        self.spec.add_dirs_rw.push(path.into());
        self
    }

    /// Grant read-only access to a path (repeatable).
    pub fn grant_ro(mut self, path: impl Into<String>) -> Self {
        self.spec.add_dirs_ro.push(path.into());
        self
    }

    /// Grant the auto-added cwd read-only instead of read-write.
    pub fn cwd_ro(mut self, on: bool) -> Self {
        self.spec.cwd_ro = on;
        self
    }

    /// Replace `$HOME` for the confined process.
    pub fn home(mut self, path: impl Into<String>) -> Self {
        self.spec.home = Some(path.into());
        self
    }

    /// Skip seeding real-home files into the (replacement) home.
    pub fn no_seed(mut self) -> Self {
        self.spec.no_seed = true;
        self
    }

    /// Pass named host env vars through to the confined process.
    pub fn env_pass(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.spec.env_pass.extend(names.into_iter().map(Into::into));
        self
    }

    /// Set an explicit `K=V` env entry (repeatable; highest precedence).
    pub fn set_env(mut self, kv: impl Into<String>) -> Self {
        self.spec.set_env.push(kv.into());
        self
    }

    /// Finalize the [`Spec`] with the command to run.
    fn spec_with(mut self, cmd: impl IntoIterator<Item = impl Into<String>>) -> Spec {
        self.spec.cmd = cmd.into_iter().map(Into::into).collect();
        self.spec
    }

    /// Launch `cmd` confined and return a non-blocking [`SandboxChild`].
    pub fn spawn(self, cmd: impl IntoIterator<Item = impl Into<String>>) -> Result<SandboxChild> {
        ensure_not_nested()?;
        let spec = self.spec_with(cmd);
        let mut eff = crate::resolve::effective_policy(&spec)?;
        crate::home::seed(&eff.home)?;
        crate::resolve::confine_executable(&mut eff.profile, &mut eff.cmd)?;
        crate::backends::select().spawn(&eff.profile, &eff.env, &eff.cmd)
    }

    /// Launch `cmd` confined and block until it exits, returning its exit code.
    pub fn run(self, cmd: impl IntoIterator<Item = impl Into<String>>) -> Result<i32> {
        self.spawn(cmd)?.wait()
    }

    /// Resolve + render the effective policy for `cmd` without spawning.
    pub fn dry_run(self, cmd: impl IntoIterator<Item = impl Into<String>>) -> Result<DryRun> {
        dry_run(&self.spec_with(cmd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_maps_to_spec() {
        let spec = Sandbox::new()
            .profile("base")
            .profile_path("/x")
            .auto_profiles(true)
            .grant_rw("/rw")
            .grant_ro("/ro")
            .home("/h")
            .no_seed()
            .cwd_ro(true)
            .env_pass(["TERM"])
            .set_env("K=V")
            .spec_with(["echo", "hi"]);
        assert_eq!(spec.profiles, ["base"]);
        assert_eq!(spec.profile_paths, ["/x"]);
        assert!(spec.auto_profiles);
        assert_eq!(spec.add_dirs_rw, ["/rw"]);
        assert_eq!(spec.add_dirs_ro, ["/ro"]);
        assert_eq!(spec.home.as_deref(), Some("/h"));
        assert!(spec.no_seed);
        assert!(spec.cwd_ro);
        assert_eq!(spec.env_pass, ["TERM"]);
        assert_eq!(spec.set_env, ["K=V"]);
        assert_eq!(spec.cmd, ["echo", "hi"]);
    }

    // Exercises the full builder → resolve → seed → confine → spawn → wait path
    // against the real Seatbelt backend; base + system-runtime let `echo` launch.
    #[cfg(target_os = "macos")]
    #[test]
    fn run_echo_exits_zero() {
        let code = Sandbox::new()
            .profile("base")
            .profile("macos/system-runtime")
            .run(["echo", "hi"])
            .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn dry_run_produces_policy_and_layer_stack() {
        let spec = Spec {
            profiles: vec!["base".into()],
            cmd: vec!["echo".into(), "hi".into()],
            ..Default::default()
        };
        let dry = dry_run(&spec).unwrap();
        assert!(
            dry.layer_names.iter().any(|(n, _)| n == "base"),
            "layer stack should include base: {:?}",
            dry.layer_names
        );
        assert!(!dry.policy.is_empty(), "rendered policy must be non-empty");
        assert_eq!(dry.cmd, vec!["echo", "hi"]);
    }
}
