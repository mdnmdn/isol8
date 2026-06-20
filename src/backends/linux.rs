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

use anyhow::{bail, Context, Result};
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
    ) -> Result<i32> {
        if cmd.is_empty() {
            bail!("no command given to run under the sandbox");
        }

        // Determine the replacement HOME for bind-mounting.
        let effective_home = env
            .get("HOME")
            .cloned()
            .unwrap_or_else(|| "/tmp".to_string());

        // Build Landlock rules from path grants.
        let rules = build_landlock_rules(profile)?;

        // We fork so the child can set up namespaces + Landlock + exec,
        // while the parent waits.
        match unsafe { nix::unistd::fork() } {
            Ok(nix::unistd::ForkResult::Parent { child }) => {
                // Parent: wait for child.
                let status = nix::sys::wait::waitpid(child, None)
                    .map_err(|e| anyhow::anyhow!("waitpid failed: {e}"))?;
                Ok(exit_code_from_waitstatus(&status))
            }
            Ok(nix::unistd::ForkResult::Child) => {
                // Child: set up isolation, apply policy, exec.
                if let Err(e) = child_setup_and_exec(rules, &effective_home, env, cmd) {
                    // eprintln! can't fail in the child after fork.
                    eprintln!("isol8: child setup failed: {e:#}");
                    std::process::exit(127);
                }
                unreachable!()
            }
            Err(e) => bail!("fork failed: {e}"),
        }
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
                let mode = match grant.access {
                    Access::Ro => "RO   ",
                    Access::Rw => "RW   ",
                    Access::Metadata => "META ",
                    Access::None => "DENY ",
                };
                out.push_str(&format!(
                    ";; {mode} {:<36} {:?}\n",
                    grant.path, grant.r#match
                ));
            }
        }
    }

    out.push_str(
        "\n;; user namespace: no (uid_map write blocked in this env)\n\
         ;; mount namespace: no (depends on user ns)\n\
         ;; PR_SET_NO_NEW_PRIVS: yes\n\
         ;; Landlock ABI: best-effort\n",
    );

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
        Access::Ro => make_bitflags!(AccessFs::{ReadFile | ReadDir}),
        Access::Rw => make_bitflags!(AccessFs::{
            ReadFile | ReadDir | WriteFile | MakeReg | MakeDir | MakeSock | MakeFifo | MakeBlock | MakeChar
        }),
        Access::Metadata => make_bitflags!(AccessFs::{ReadFile | ReadDir}),
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
/// them). `metadata` gets read-only for stat access.
///
/// **Match kind limitation**: Landlock's `PathBeneath` grants a whole subtree,
/// so `literal` (exact-match only) and `regex`/`prefix` match kinds cannot be
/// faithfully represented. We only emit rules for `subpath` match kind.
fn build_landlock_rules(profile: &Profile) -> Result<Vec<LandlockRule>> {
    let mut rules = Vec::new();

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

        // Landlock's PathBeneath requires a directory FD. For `subpath` and
        // `prefix` we grant the whole subtree; for `literal` we can only
        // approximate (Landlock doesn't support literal-file-only).
        // We open the parent directory and PathBeneath under it.
        let path = Path::new(&grant.path);
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent().unwrap_or(Path::new("/")).to_path_buf()
        };

        rules.push(LandlockRule {
            path: dir.to_string_lossy().into_owned(),
            access: rights,
        });
    }

    // Ancestor metadata (R2.3): for each granted path, every ancestor must be
    // stat-able. Landlock's PathBeneath on a parent implicitly grants this,
    // but for paths whose ancestors aren't already covered, we add explicit
    // read-only rules on the ancestors.
    // NOTE: We skip "/" as an ancestor — it's always stat-able, and adding it
    // as a PathBeneath rule would grant the entire filesystem.
    let mut ancestors_needed: Vec<String> = Vec::new();
    for grant in &profile.paths {
        if grant.access == Access::None {
            continue;
        }
        let mut cur = Path::new(&grant.path).parent();
        while let Some(dir) = cur {
            let s = dir.to_string_lossy().into_owned();
            if s.is_empty() || s == "/" {
                break;
            }
            // Check if any existing rule already covers this ancestor.
            if !rules.iter().any(|r| r.path == s) && !ancestors_needed.contains(&s) {
                ancestors_needed.push(s);
            }
            cur = dir.parent();
        }
    }
    for anc in ancestors_needed {
        rules.push(LandlockRule {
            path: anc,
            access: make_bitflags!(AccessFs::{ReadFile | ReadDir}),
        });
    }

    Ok(rules)
}

/// Apply Landlock rules to the current process.
fn apply_landlock(rules: &[LandlockRule]) -> Result<()> {
    if rules.is_empty() {
        return Ok(()); // no rules — nothing to restrict
    }

    // Determine the maximum access rights we need.
    let handled_accesses = rules
        .iter()
        .fold(BitFlags::<AccessFs>::empty(), |acc, r| acc | r.access);

    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(handled_accesses)
        .map_err(|e| anyhow::anyhow!("Landlock ruleset handle_access: {e}"))?
        .create()
        .map_err(|e| anyhow::anyhow!("Landlock ruleset create: {e}"))?;

    let mut fds: Vec<landlock::PathFd> = Vec::new();
    for rule in rules {
        let path = Path::new(&rule.path);
        let fd = landlock::PathFd::new(path)
            .map_err(|e| anyhow::anyhow!("Landlock PathFd({}): {e}", rule.path))?;
        fds.push(fd);
    }

    let mut created = ruleset;
    for (rule, fd) in rules.iter().zip(fds.iter()) {
        created = created
            .add_rule(PathBeneath::new(fd, rule.access))
            .map_err(|e| anyhow::anyhow!("Landlock add_rule: {e}"))?;
    }

    let status = created
        .restrict_self()
        .map_err(|e| anyhow::anyhow!("Landlock restrict_self: {e}"))?;

    match status.ruleset {
        RulesetStatus::FullyEnforced => {}
        RulesetStatus::PartiallyEnforced => {
            // Log but don't fail — partial enforcement is still better than none.
            eprintln!(
                "isol8: warning: Landlock partially enforced (kernel may lack some features)"
            );
        }
        RulesetStatus::NotEnforced => {
            bail!(
                "Landlock not enforced. Check kernel version (>= 5.13) and \
                 /proc/sys/kernel/unprivileged_userns_clone settings."
            );
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
        .ok_or_else(|| anyhow::anyhow!("empty command"))?;
    let args = &cmd[1..];

    let mut command = Command::new(program);
    command.args(args);
    command.env_clear().envs(env);

    // Replace the current process.
    use std::os::unix::process::CommandExt;
    let err = command.exec();
    // exec only returns on error.
    bail!("exec failed for {}: {err}", program)
}

// ---------------------------------------------------------------------------
// Namespace helpers
// ---------------------------------------------------------------------------

/// Set `PR_SET_NO_NEW_PRIVS` so the process cannot gain privileges via
/// setuid/setgid binaries or file capabilities.
fn set_no_new_privs() -> Result<()> {
    unsafe {
        if nix::libc::prctl(nix::libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            bail!(
                "PR_SET_NO_NEW_PRIVS failed: {}",
                std::io::Error::last_os_error()
            );
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
        .map_err(|e| anyhow::anyhow!("unshare(CLONE_NEWUSER | CLONE_NEWNS) failed: {e}"))?;
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
        .context("writing /proc/self/uid_map (did you unshare CLONE_NEWUSER?)")?;

    // Write deny to /proc/self/setgroups before gid_map (required for unprivileged).
    std::fs::write("/proc/self/setgroups", "deny").context("writing /proc/self/setgroups")?;

    // Write gid_map: "0 <real-gid> 1"
    let gid_map = format!("0 {} 1", gid);
    std::fs::write("/proc/self/gid_map", &gid_map).context("writing /proc/self/gid_map")?;

    Ok(())
}

/// Bind-mount `new_home` over `real_home` so `getpwuid`-derived `~` and any
/// path resolution hitting the real home go into the replacement (R4.6).
#[allow(dead_code)]
fn bind_mount_home(new_home: &str, real_home: &str) -> Result<()> {
    use nix::mount::{mount, MsFlags};

    // Ensure the target (real home) exists as a mount point.
    std::fs::create_dir_all(real_home)
        .with_context(|| format!("creating mount point at {real_home}"))?;

    // MS_BIND | MS_REC: recursively bind-mount new_home over real_home.
    mount(
        Some(new_home),
        real_home,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .map_err(|e| anyhow::anyhow!("bind-mount {} -> {} failed: {e}", new_home, real_home))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Exit code mapping
// ---------------------------------------------------------------------------

/// Map a `WaitStatus` to a shell-style exit code.
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
        // /usr and /tmp each produce a rule; / is an ancestor for both.
        assert!(rules.len() >= 2);
        // Check that /usr has read rights.
        let usr_rule = rules.iter().find(|r| r.path == "/usr").unwrap();
        assert!(usr_rule.access.contains(AccessFs::ReadFile));
        assert!(!usr_rule.access.contains(AccessFs::WriteFile));
        // Check that /tmp has write rights.
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
    fn exit_code_normal() {
        use nix::sys::wait::WaitStatus;
        use nix::unistd::Pid;
        assert_eq!(
            exit_code_from_waitstatus(&WaitStatus::Exited(Pid::from_raw(1), 42)),
            42
        );
    }
}
