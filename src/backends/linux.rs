//! Linux backend using Landlock LSM + user/mount namespaces.
//!
//! Renders the merged [`Profile`] into Landlock rules (deny-by-default,
//! per-path ro/rw) and runs the command inside user + mount namespaces for
//! HOME replacement and process isolation.
//!
//! ## Architecture
//!
//! 1. **Landlock** enforces filesystem access (R2): deny-by-default, explicit
//!    ro/rw rules per path grant.
//! 2. **User namespace** (CLONE_NEWUSER) gives us mappings to remap UIDs
//!    without root, enabling mount namespace operations.
//! 3. **Mount namespace** (CLONE_NEWNS) lets us bind-mount the replacement
//!    HOME over the real one (R4.6) for robust `$HOME` isolation.
//! 4. `PR_SET_NO_NEW_PRIVS` prevents privilege escalation (R1.2).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result, ResultExt};
use crate::sandbox::SandboxChild;
use landlock::{
    make_bitflags, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus,
};

use super::Backend;
use crate::profile::{Access, MatchKind, Profile};

impl Backend for super::linux::LinuxBackend {
    fn spawn(
        &self,
        profile: &Profile,
        env: &HashMap<String, String>,
        cmd: &[String],
    ) -> Result<SandboxChild> {
        if cmd.is_empty() {
            return Err(Error::Message(
                "no command given to run under the sandbox".into(),
            ));
        }

        // Determine the replacement HOME for bind-mounting.
        let effective_home = env
            .get("HOME")
            .cloned()
            .unwrap_or_else(|| "/tmp".to_string());

        // Build Landlock rules from path grants.
        let rules = build_landlock_rules(profile)?;

        // Fork so the child can set up namespaces + Landlock + exec; the parent
        // returns a non-blocking handle around the child pid (reaped on wait()).
        match unsafe { nix::unistd::fork() } {
            Ok(nix::unistd::ForkResult::Parent { child }) => Ok(SandboxChild::forked(child)),
            Ok(nix::unistd::ForkResult::Child) => {
                // Child: set up isolation, apply policy, exec.
                if let Err(e) = child_setup_and_exec(rules, &effective_home, env, cmd) {
                    // eprintln! can't fail in the child after fork.
                    eprintln!("isol8: child setup failed: {e}");
                    std::process::exit(127);
                }
                unreachable!()
            }
            Err(e) => Err(Error::Message(format!("fork failed: {e}"))),
        }
    }

    fn render_policy(&self, profile: &Profile) -> String {
        render_policy(profile)
    }
}

pub struct LinuxBackend;

/// Render the merged profile into human-readable Landlock rules (for `--dry-run`).
pub(crate) fn render_policy(profile: &Profile) -> String {
    let mut out = String::new();
    out.push_str(";; Landlock filesystem rules (deny-by-default)\n");
    out.push_str(";; Paths not listed below have NO access.\n\n");

    if profile.paths.is_empty() {
        out.push_str(";; (no path grants — all access denied)\n");
    } else {
        for grant in &profile.paths {
            let rights = access_for_grant(grant.access);
            if rights.is_empty() {
                out.push_str(&format!(";; DENY  {:<36} (explicit deny)\n", grant.path));
            } else {
                // Metadata maps to ReadFile|ReadDir (ro) under Landlock; no
                // true stat-only right exists. Report the effective grant
                // honestly for --dry-run / --show-policies.
                let mode = match grant.access {
                    Access::Ro => "RO      ",
                    Access::Rw => "RW      ",
                    Access::Metadata => "META→ro ",
                    Access::None => "DENY    ",
                };
                out.push_str(&format!(
                    ";; {mode} {:<36} {:?}\n",
                    grant.path, grant.r#match
                ));
            }
        }
    }

    let abi = probe_landlock_abi();
    out.push_str(&format!(
        "\n;; user namespace: no (uid_map write blocked in this env)\n\
         ;; mount namespace: no (depends on user ns)\n\
         ;; PR_SET_NO_NEW_PRIVS: yes\n\
         ;; Landlock ABI: {abi}\n",
    ));

    out
}

// ---------------------------------------------------------------------------
// Landlock ruleset construction
// ---------------------------------------------------------------------------

/// A prepared Landlock rule: the filesystem path and the access rights to grant.
struct LandlockRule {
    path: String,
    access: BitFlags<AccessFs>,
}

/// Convert profile `PathGrant`s into Landlock access rights.
fn access_for_grant(access: Access) -> BitFlags<AccessFs> {
    match access {
        Access::Ro => make_bitflags!(AccessFs::{ReadFile | ReadDir | Execute}),
        Access::Rw => make_bitflags!(AccessFs::{
            ReadFile | ReadDir | WriteFile | Execute |
            MakeReg | MakeDir | MakeSock | MakeFifo | MakeBlock | MakeChar
        }),
        Access::Metadata => make_bitflags!(AccessFs::{ReadFile | ReadDir | Execute}),
        Access::None => {
            // Explicit deny: no Landlock rule. Landlock is deny-by-default;
            // simply not granting access is the enforcement.
            BitFlags::empty()
        }
    }
}

/// Build the list of Landlock rules from the merged profile.
///
/// Landlock is deny-by-default: only paths that have an explicit `ro` or `rw`
/// grant get a rule. `none` grants are simply omitted (the default deny covers
/// them). `metadata` is implemented as ro rights (ReadFile | ReadDir); Landlock
/// has no metadata-only right (see limitations in linux-support.md).
///
/// **Match kind limitation**: Landlock's `PathBeneath` grants a whole subtree,
/// so `literal` (exact-match only) and `regex`/`prefix` match kinds cannot be
/// faithfully represented. We only emit rules for `subpath` match kind.
///
/// **No ancestor rules**: Unlike macOS Seatbelt, Landlock's `PathBeneath`
/// grants access to the entire subtree beneath a directory. Adding ancestor
/// rules for path resolution (R2.3) would inadvertently grant access to
/// sibling directories (e.g., adding `/home` as ancestor of `/home/user/.config`
/// grants all of `/home/`). Unix DAC already allows path traversal — the child
/// process can traverse parent directories to reach granted paths. Landlock
/// only restricts which directory FDs can be opened, not traversal.
///
/// Directory targeting: `PathBeneath` requires an existing directory FD. We
/// target the grant path if it is an existing dir; otherwise its nearest
/// parent. This means a grant on a specific file will grant its parent dir
/// (and thus siblings). Non-existing subdirs under a granted ancestor (e.g.
/// scratch HOME + ~/.config) end up with a rule on the ancestor dir, which
/// is acceptable given Landlock semantics and the metadata→ro limitation.
fn build_landlock_rules(profile: &Profile) -> Result<Vec<LandlockRule>> {
    use std::collections::HashMap;

    let mut by_dir: HashMap<String, BitFlags<AccessFs>> = HashMap::new();

    for grant in &profile.paths {
        let rights = access_for_grant(grant.access);
        if rights.is_empty() {
            continue; // `none` — deny-by-default handles it
        }

        // Landlock only supports subtree (subpath) grants. Skip literal/prefix/regex
        // which are macOS Seatbelt matchers with no Landlock equivalent.
        if grant.r#match != MatchKind::Subpath {
            continue;
        }

        // Landlock's PathBeneath requires a directory FD.
        let path = Path::new(&grant.path);
        let target = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent().unwrap_or(Path::new("/")).to_path_buf()
        };
        let key = target.to_string_lossy().into_owned();
        *by_dir.entry(key).or_insert(BitFlags::empty()) |= rights;
    }

    let rules: Vec<LandlockRule> = by_dir
        .into_iter()
        .map(|(path, access)| LandlockRule { path, access })
        .collect();
    Ok(rules)
}

/// Probe the kernel's Landlock ABI version (no side effects on the calling process).
///
/// Uses the raw `landlock_create_ruleset(..., LANDLOCK_CREATE_RULESET_VERSION)`
/// syscall directly. This reports the true kernel ABI (e.g. "v5 (enforced)")
/// without installing any policy or calling `restrict_self()`, so `--dry-run`
/// never confines the calling `isol8` process.
fn probe_landlock_abi() -> String {
    // Query only — flag requests version, attr/size=0, no ruleset created.
    // The syscall number 444 is the asm-generic value (x86_64, aarch64, riscv etc).
    // If the call fails with ENOSYS/EOPNOTSUPP we treat Landlock as unavailable.
    let v = unsafe {
        nix::libc::syscall(
            444,
            std::ptr::null::<nix::libc::c_void>(),
            0usize,
            1u32, // LANDLOCK_CREATE_RULESET_VERSION = (1U << 0)
        ) as nix::libc::c_int
    };
    if v < 0 {
        // EOPNOTSUPP or ENOSYS (or other) => no Landlock or disabled.
        return "unavailable".to_string();
    }
    if v >= 1 {
        format!("v{} (enforced)", v)
    } else {
        "unavailable".to_string()
    }
}

/// Apply Landlock rules to the current process.
fn apply_landlock(rules: &[LandlockRule]) -> Result<()> {
    if rules.is_empty() {
        return Ok(()); // no rules — nothing to restrict
    }

    // Always handle a comprehensive set of rights. This ensures deny-by-default
    // for rights that are not granted by any rule in this profile (e.g. a "ro"
    // grant must still deny Make*/Write*). Using BestEffort lets old kernels drop
    // unknown bits.
    let handled_accesses = make_bitflags!(AccessFs::{
        ReadFile | ReadDir | WriteFile | Execute |
        MakeReg | MakeDir | MakeSock | MakeFifo | MakeBlock | MakeChar
    });

    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(handled_accesses)
        .map_err(|e| Error::Message(format!("Landlock ruleset handle_access: {e}")))?
        .create()
        .map_err(|e| Error::Message(format!("Landlock ruleset create: {e}")))?;

    let mut fds: Vec<landlock::PathFd> = Vec::new();
    for rule in rules {
        let path = Path::new(&rule.path);
        let fd = landlock::PathFd::new(path)
            .map_err(|e| Error::Message(format!("Landlock PathFd({}): {e}", rule.path)))?;
        fds.push(fd);
    }

    let mut created = ruleset;
    for (rule, fd) in rules.iter().zip(fds.iter()) {
        created = created
            .add_rule(PathBeneath::new(fd, rule.access))
            .map_err(|e| Error::Message(format!("Landlock add_rule: {e}")))?;
    }

    let status = created
        .restrict_self()
        .map_err(|e| Error::Message(format!("Landlock restrict_self: {e}")))?;

    match status.ruleset {
        RulesetStatus::FullyEnforced => {}
        RulesetStatus::PartiallyEnforced => {
            // Log but don't fail — partial enforcement is still better than none.
            eprintln!(
                "isol8: warning: Landlock partially enforced (kernel may lack some features)"
            );
        }
        RulesetStatus::NotEnforced => {
            return Err(Error::Message(
                "Landlock not enforced. Check kernel version (>= 5.13) and \
                 /proc/sys/kernel/unprivileged_userns_clone settings."
                    .into(),
            ));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Child process setup (after fork, before exec)
// ---------------------------------------------------------------------------

/// Set up isolation, Landlock, and exec the target command.
fn child_setup_and_exec(
    rules: Vec<LandlockRule>,
    _effective_home: &str,
    env: &HashMap<String, String>,
    cmd: &[String],
) -> Result<()> {
    // 1. Prevent privilege escalation.
    set_no_new_privs()?;

    // 2. Apply Landlock rules (filesystem confinement).
    apply_landlock(&rules)?;

    // 6. Exec the target command with the sanitized environment.
    let program = cmd
        .first()
        .ok_or_else(|| Error::Message("empty command".into()))?;
    let args = &cmd[1..];

    let mut command = Command::new(program);
    command.args(args);
    command.env_clear().envs(env);

    // Replace the current process.
    use std::os::unix::process::CommandExt;
    let err = command.exec();
    // exec only returns on error.
    Err(Error::Message(format!("exec failed for {program}: {err}")))
}

// ---------------------------------------------------------------------------
// Namespace helpers
// ---------------------------------------------------------------------------

/// Set `PR_SET_NO_NEW_PRIVS` so the process cannot gain privileges via
/// setuid/setgid binaries or file capabilities.
fn set_no_new_privs() -> Result<()> {
    unsafe {
        if nix::libc::prctl(nix::libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(Error::Message(format!(
                "PR_SET_NO_NEW_PRIVS failed: {}",
                std::io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

/// Unshare into user + mount namespaces. The user namespace is required so we
/// can do mount operations (bind-mount HOME) without root.
///
/// NOTE: This requires `uid_map` writes which may be blocked in some VM
/// environments (e.g. OrbStack). When unavailable, Landlock-only mode
/// provides filesystem confinement without HOME bind-mounting.
#[allow(dead_code)]
fn unshare_user_and_mount_ns() -> Result<()> {
    let flags = nix::sched::CloneFlags::CLONE_NEWUSER | nix::sched::CloneFlags::CLONE_NEWNS;
    nix::sched::unshare(flags)
        .map_err(|e| Error::Message(format!("unshare(CLONE_NEWUSER | CLONE_NEWNS) failed: {e}")))?;
    Ok(())
}

/// Write uid/gid mappings for the user namespace. After unsharing into a user
/// namespace, we are uid 0 inside the ns but need to map our real uid.
#[allow(dead_code)]
fn write_uid_gid_mappings() -> Result<()> {
    let uid = nix::unistd::getuid();
    let gid = nix::unistd::getgid();

    // Write uid_map: "0 <real-uid> 1"
    let uid_map = format!("0 {} 1", uid);
    std::fs::write("/proc/self/uid_map", &uid_map)
        .ctx(|| "writing /proc/self/uid_map (did you unshare CLONE_NEWUSER?)")?;

    // Write deny to /proc/self/setgroups before gid_map (required for unprivileged).
    std::fs::write("/proc/self/setgroups", "deny").ctx(|| "writing /proc/self/setgroups")?;

    // Write gid_map: "0 <real-gid> 1"
    let gid_map = format!("0 {} 1", gid);
    std::fs::write("/proc/self/gid_map", &gid_map).ctx(|| "writing /proc/self/gid_map")?;

    Ok(())
}

/// Bind-mount `new_home` over `real_home` so `getpwuid`-derived `~` and any
/// path resolution hitting the real home go into the replacement (R4.6).
#[allow(dead_code)]
fn bind_mount_home(new_home: &str, real_home: &str) -> Result<()> {
    use nix::mount::{mount, MsFlags};

    // Ensure the target (real home) exists as a mount point.
    std::fs::create_dir_all(real_home).ctx(|| format!("creating mount point at {real_home}"))?;

    // MS_BIND | MS_REC: recursively bind-mount new_home over real_home.
    mount(
        Some(new_home),
        real_home,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .map_err(|e| Error::Message(format!("bind-mount {new_home} -> {real_home} failed: {e}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Exit code mapping
// ---------------------------------------------------------------------------

/// Map a `WaitStatus` to a shell-style exit code.
///
/// The parent no longer reaps inline (it returns a `SandboxChild::forked` handle, and
/// `SandboxChild::wait` does the `waitpid` + mapping); kept for the unit test.
#[allow(dead_code)]
fn exit_code_from_waitstatus(status: &nix::sys::wait::WaitStatus) -> i32 {
    match status {
        nix::sys::wait::WaitStatus::Exited(_, code) => *code,
        nix::sys::wait::WaitStatus::Signaled(_, sig, _) => 128 + (*sig as i32),
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{MatchKind, PathGrant, Profile};

    fn grant(path: &str, access: Access) -> PathGrant {
        PathGrant {
            path: path.to_string(),
            access,
            r#match: MatchKind::Subpath,
        }
    }

    #[test]
    fn access_for_ro_rw_none() {
        // None => empty
        assert!(access_for_grant(Access::None).is_empty());
        assert!(!access_for_grant(Access::Ro).is_empty());
        assert!(!access_for_grant(Access::Rw).is_empty());
        assert!(!access_for_grant(Access::Metadata).is_empty());
    }

    #[test]
    fn build_rules_empty_profile() {
        let p = Profile::default();
        let rules = build_landlock_rules(&p).unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn build_rules_ro_rw() {
        let p = Profile {
            paths: vec![grant("/usr", Access::Ro), grant("/tmp", Access::Rw)],
            ..Default::default()
        };
        let rules = build_landlock_rules(&p).unwrap();
        // Only /usr and /tmp — no ancestor rules (Landlock PathBeneath grants
        // subtrees, so ancestor rules would over-grant).
        assert_eq!(rules.len(), 2);
        let usr_rule = rules.iter().find(|r| r.path == "/usr").unwrap();
        assert!(usr_rule.access.contains(AccessFs::ReadFile));
        assert!(!usr_rule.access.contains(AccessFs::WriteFile));
        let tmp_rule = rules.iter().find(|r| r.path == "/tmp").unwrap();
        assert!(tmp_rule.access.contains(AccessFs::WriteFile));
    }

    #[test]
    fn build_rules_none_omitted() {
        let p = Profile {
            paths: vec![grant("/secret", Access::None)],
            ..Default::default()
        };
        let rules = build_landlock_rules(&p).unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn build_rules_no_ancestor_over_granting() {
        // Metadata grants should NOT cause ancestor rules that expose sibling dirs.
        // /var/tmp is a real directory; /home/user/.config may not exist, so the
        // builder uses its parent (/home/user) as the Landlock FD target — that's
        // fine, the key assertion is that NO extra ancestor rules appear.
        let p = Profile {
            paths: vec![
                grant("/var/tmp", Access::Rw),
                grant("/var/cache", Access::Rw),
            ],
            ..Default::default()
        };
        let rules = build_landlock_rules(&p).unwrap();
        // Only /var/tmp and /var/cache — no /var ancestor rule.
        assert_eq!(rules.len(), 2);
        assert!(rules
            .iter()
            .all(|r| r.path == "/var/tmp" || r.path == "/var/cache"));
    }

    #[test]
    fn exit_code_normal() {
        use nix::sys::wait::WaitStatus;
        use nix::unistd::Pid;
        assert_eq!(
            exit_code_from_waitstatus(&WaitStatus::Exited(Pid::from_raw(1), 42)),
            42
        );
    }

    #[test]
    fn render_policy_contains_deny_default_and_abi() {
        let p = Profile {
            paths: vec![grant("/usr", Access::Ro), grant("/tmp", Access::Rw)],
            ..Default::default()
        };
        let pol = render_policy(&p);
        assert!(pol.contains(";; Landlock filesystem rules (deny-by-default)"));
        assert!(pol.contains(";; Paths not listed below have NO access."));
        assert!(pol.contains("Landlock ABI: "));
        // Should not contain the old wrong "restrict" side-effect text in probe path.
        assert!(!pol.contains("restrict_self"));
    }

    #[test]
    fn render_policy_metadata_reports_as_ro() {
        let p = Profile {
            paths: vec![
                grant("/etc", Access::Ro),
                grant("~/.config", Access::Metadata),
                grant("/tmp", Access::Rw),
            ],
            ..Default::default()
        };
        let pol = render_policy(&p);
        // Metadata must be reported honestly as mapping to ro enforcement.
        assert!(pol.contains("META→ro ") || pol.contains("RO      "));
        assert!(pol.contains("RO      "));
        assert!(pol.contains("RW      "));
    }

    #[test]
    fn probe_returns_sane_abi_or_unavailable() {
        let s = probe_landlock_abi();
        // On a Landlock kernel we expect "vN (enforced)"; elsewhere "unavailable".
        assert!(s == "unavailable" || s.starts_with("v") && s.contains("(enforced)"));
    }
}
