//! macOS Seatbelt (`sandbox-exec`) backend.
//!
//! Renders the merged [`Profile`] into an SBPL policy string and runs the command
//! under `/usr/bin/sandbox-exec -p <policy>`. Deny-by-default: the policy opens with
//! `(deny default)` and only the explicit grants widen it.
//!
//! ## Validated behavior (real `sandbox-exec`, macOS 26)
//!
//! - **Minimal allow-set for a trivial command to start** (no imports, fully
//!   self-contained — Agent C can build `profiles/macos-system.toml` on this):
//!   ```text
//!   (version 1)
//!   (deny default)
//!   (allow process-exec*)
//!   (allow process-fork)
//!   (allow file-read* (subpath "/usr/lib") (subpath "/System") (subpath "/bin") (literal "/"))
//!   ```
//!   `(literal "/")` is mandatory — every process inherits cwd `/` from launchd and
//!   reads it on start; without it the runtime aborts with SIGABRT (exit 134).
//!   `/System` must be the whole subtree (it holds the dyld shared cache under
//!   `/System/Volumes/Preboot/Cryptexes`). `/usr/lib` covers dylibs; `/bin`,
//!   `/usr/bin` cover the binaries themselves.
//!
//! - **`none` (explicit deny / hole):** on macOS Seatbelt the *last* matching rule
//!   wins, so a deny must be emitted **after** the allows to carve a hole out of a
//!   broader grant — and it must name the concrete operation classes
//!   `file-read* file-write*`. A bare `(deny file* …)` does **not** block writes
//!   (verified: the write still succeeded), so we never use it.
//!
//! - **Ancestor metadata (R2.3):** path resolution stat()s every ancestor of a
//!   granted path; without `file-read-metadata` on them, `getcwd`/`open` fail. We
//!   emit `(allow file-read-metadata (literal "<ancestor>"))` for each ancestor.

use std::collections::HashMap;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::Backend;
use crate::profile::{Access, Capability, MatchKind, Profile};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub struct MacosBackend;

impl Backend for MacosBackend {
    fn spawn(
        &self,
        profile: &Profile,
        env: &HashMap<String, String>,
        cmd: &[String],
    ) -> Result<i32> {
        if cmd.is_empty() {
            bail!("no command given to run under the sandbox");
        }

        let policy = render_policy(profile);

        // ponytail: -p inline policy string, no temp .sb file.
        let mut command = Command::new(SANDBOX_EXEC);
        command.arg("-p").arg(&policy).args(cmd);
        command.env_clear().envs(env);

        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "{SANDBOX_EXEC} not found. Seatbelt is deprecated but present on \
                     macOS 12+; isol8 requires it for the macOS backend."
                )
            } else {
                anyhow::Error::new(e).context(format!("failed to launch {SANDBOX_EXEC}"))
            }
        })?;

        let status = child
            .wait()
            .with_context(|| format!("waiting for {SANDBOX_EXEC}"))?;

        // sandbox-exec uses exit 64/65/71 for its own usage/policy/exec failures.
        match status.code() {
            Some(64) => bail!(
                "sandbox-exec reported a usage error (exit 64). Check that the confined \
                 command and arguments are valid."
            ),
            Some(65) => bail!(
                "sandbox-exec rejected the generated Seatbelt policy (exit 65). This is \
                 a policy-compile error, not the command failing. Generated policy:\n\
                 ----\n{policy}\n----\n\
                 Re-run with --show-policies to inspect the effective policy."
            ),
            Some(71) => bail!(
                "sandbox-exec failed to execute the confined command (exit 71). The \
                 command may be missing or not executable."
            ),
            _ => {}
        }

        Ok(exit_code(&status))
    }
}

/// Map a child `ExitStatus` to a shell-style exit code: the real code, or 128+signo
/// if signal-terminated, else 1.
fn exit_code(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        128 + sig
    } else {
        1
    }
}

/// Render the merged profile into an SBPL policy string.
///
/// Shape (order matters — Seatbelt is last-match-wins):
/// 1. header `(version 1) (deny default)`
/// 2. ancestor `file-read-metadata` grants (deduped) for path resolution (R2.3)
/// 3. per-grant allows (`ro`/`rw`/`metadata`)
/// 4. per-grant `none` denies (after the allows, so they carve holes)
/// 5. capability allows
/// 6. raw SBPL passthrough, verbatim
pub(crate) fn render_policy(profile: &Profile) -> String {
    let mut out = String::from("(version 1)\n(deny default)\n");

    // macOS firmlinks/symlinks (`/tmp`->`/private/tmp`, `/var`->`/private/var`,
    // `/home`->`/System/Volumes/Data/home`, …) are NOT interchangeable to Seatbelt:
    // a grant on one form does not match an access via the other. So for each grant
    // we emit BOTH the authored path and its symlink-resolved form (tools open
    // either). `targets[i]` is the deduped target list for `profile.paths[i]`.
    let targets: Vec<Vec<String>> = profile
        .paths
        .iter()
        .map(|g| {
            let resolved = resolve_symlinks(&g.path);
            if resolved == g.path {
                vec![g.path.clone()]
            } else {
                vec![g.path.clone(), resolved]
            }
        })
        .collect();

    // 2. Ancestor metadata, deduped across all grants (R2.3). A granted path needs
    //    every ancestor stat-able for resolution, without granting their content.
    let mut ancestors: Vec<String> = Vec::new();
    for grant_targets in &targets {
        for path in grant_targets {
            for anc in ancestor_dirs(path) {
                if !ancestors.contains(&anc) {
                    ancestors.push(anc);
                }
            }
        }
    }
    if !ancestors.is_empty() {
        out.push_str(";; ancestor metadata for path resolution (R2.3)\n");
        for anc in &ancestors {
            out.push_str(&format!(
                "(allow file-read-metadata (literal {}))\n",
                sbpl_string(anc)
            ));
        }
    }

    // 3. Allows (skip `none` here).
    out.push_str(";; path grants\n");
    for (grant, grant_targets) in profile.paths.iter().zip(&targets) {
        let m = matchers(grant.r#match, grant_targets);
        match grant.access {
            Access::Ro => {
                out.push_str(&format!("(allow file-read* {m})\n"));
            }
            Access::Rw => {
                out.push_str(&format!("(allow file-read* {m})\n"));
                out.push_str(&format!("(allow file-write* {m})\n"));
            }
            Access::Metadata => {
                out.push_str(&format!("(allow file-read-metadata {m})\n"));
            }
            Access::None => {}
        }
    }

    // 4. `none` denies, AFTER the allows so they carve holes (last-match-wins).
    //    Bare `file*` does NOT block writes — we name `file-read* file-write*`.
    let has_none = profile.paths.iter().any(|g| g.access == Access::None);
    if has_none {
        out.push_str(";; explicit denies (carve holes; emitted after allows)\n");
        for (grant, grant_targets) in profile.paths.iter().zip(&targets) {
            if grant.access == Access::None {
                let m = matchers(grant.r#match, grant_targets);
                out.push_str(&format!("(deny file-read* file-write* {m})\n"));
            }
        }
    }

    // 5. Capabilities.
    if let Some(macos) = &profile.macos {
        if !macos.capabilities.is_empty() {
            out.push_str(";; macos capabilities\n");
            for cap in &macos.capabilities {
                out.push_str(capability_rule(*cap));
                out.push('\n');
            }
        }

        // 6. Raw passthrough, verbatim.
        if !macos.raw.is_empty() {
            out.push_str(";; raw SBPL passthrough\n");
            out.push_str(&macos.raw);
            if !macos.raw.ends_with('\n') {
                out.push('\n');
            }
        }
    }

    out
}

/// Build the SBPL matcher clause `(<token> "<escaped-path>")` for a grant.
///
/// `prefix` has no native Seatbelt matcher; we approximate it with an anchored
/// regex `^<escaped>` (documented in profile-model.md §5).
fn matcher(kind: MatchKind, path: &str) -> String {
    match kind {
        MatchKind::Subpath => format!("(subpath {})", sbpl_string(path)),
        MatchKind::Literal => format!("(literal {})", sbpl_string(path)),
        MatchKind::Prefix => {
            // Anchor a regex at the start to approximate a string prefix.
            let pat = format!("^{}", regex_escape(path));
            format!("(regex {})", sbpl_string(&pat))
        }
        MatchKind::Regex => format!("(regex {})", sbpl_string(path)),
    }
}

/// Join the matcher atoms for several target paths of one grant into a single
/// filter clause, e.g. `(subpath "/a") (subpath "/private/a")` — a Seatbelt
/// `(allow op …)` accepts any number of matchers.
fn matchers(kind: MatchKind, paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| matcher(kind, p))
        .collect::<Vec<_>>()
        .join(" ")
}

/// SBPL operation rule for a typed capability.
///
/// Tokens verified to compile under real `sandbox-exec` (macOS 26). Corrections:
/// - `pasteboard` is **not** a Seatbelt operation; pasteboard access is mediated by
///   a mach-lookup to `com.apple.pboard.service`, which is what we emit.
fn capability_rule(cap: Capability) -> &'static str {
    match cap {
        Capability::MachLookup => "(allow mach-lookup)",
        Capability::MachRegister => "(allow mach-register)",
        Capability::IokitOpen => "(allow iokit-open)",
        Capability::SysctlRead => "(allow sysctl-read)",
        Capability::ProcessExec => "(allow process-exec*)",
        Capability::ProcessFork => "(allow process-fork)",
        Capability::ProcessInfo => "(allow process-info*)",
        Capability::Signal => "(allow signal)",
        Capability::PseudoTty => "(allow pseudo-tty)",
        Capability::UserPreferenceRead => "(allow user-preference-read)",
        Capability::UserPreferenceWrite => "(allow user-preference-write)",
        Capability::IpcPosixShm => "(allow ipc-posix-shm*)",
        // The kebab `sysv-sem` maps to SBPL `ipc-sysv-sem` (verified token name).
        Capability::SysvSem => "(allow ipc-sysv-sem)",
        // No `pasteboard` op exists; pasteboard is the pboard mach service.
        Capability::Pasteboard => "(allow mach-lookup (global-name \"com.apple.pboard.service\"))",
    }
}

/// Resolve symlinks in `path` so it matches what Seatbelt sees (it matches on the
/// canonical path). `canonicalize` needs the path to exist, so we canonicalize the
/// longest existing prefix and re-append the non-existent tail — this resolves
/// macOS's `/var`->`/private/var` / `/tmp`->`/private/tmp` even for a grant on a
/// dir that hasn't been created yet. Falls back to the input if nothing resolves.
fn resolve_symlinks(path: &str) -> String {
    let p = std::path::Path::new(path);
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur: &std::path::Path = p;
    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out.to_string_lossy().into_owned();
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                cur = parent;
            }
            _ => return path.to_string(), // nothing along the path resolved
        }
    }
}

/// All ancestor directories of `path`, from the topmost down to its immediate
/// parent, e.g. `/a/b/c` -> `["/", "/a", "/a/b"]`. Root itself yields nothing.
fn ancestor_dirs(path: &str) -> Vec<String> {
    let p = std::path::Path::new(path);
    let mut anc: Vec<String> = Vec::new();
    let mut cur = p.parent();
    while let Some(dir) = cur {
        match dir.to_str() {
            Some(s) if !s.is_empty() => anc.push(s.to_string()),
            _ => break,
        }
        cur = dir.parent();
    }
    anc.reverse(); // topmost ("/") first
    anc
}

/// Escape a string for an SBPL double-quoted literal: backslash and double-quote.
fn sbpl_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Escape regex metacharacters so a literal path can be embedded in an SBPL regex
/// (used to approximate `prefix`). The result is still wrapped by `sbpl_string`.
fn regex_escape(s: &str) -> String {
    const META: &[char] = &[
        '.', '^', '$', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '\\',
    ];
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if META.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{MacosExtra, PathGrant};

    fn grant(path: &str, access: Access, m: MatchKind) -> PathGrant {
        PathGrant {
            path: path.to_string(),
            access,
            r#match: m,
        }
    }

    #[test]
    fn header_is_deny_default() {
        let p = Profile::default();
        let pol = render_policy(&p);
        assert!(pol.starts_with("(version 1)\n(deny default)\n"));
    }

    #[test]
    fn ro_rw_metadata_render_expected_ops() {
        let p = Profile {
            paths: vec![
                grant("/ro", Access::Ro, MatchKind::Subpath),
                grant("/rw", Access::Rw, MatchKind::Subpath),
                grant("/m", Access::Metadata, MatchKind::Literal),
            ],
            ..Default::default()
        };
        let pol = render_policy(&p);
        assert!(pol.contains(r#"(allow file-read* (subpath "/ro"))"#));
        assert!(pol.contains(r#"(allow file-read* (subpath "/rw"))"#));
        assert!(pol.contains(r#"(allow file-write* (subpath "/rw"))"#));
        assert!(pol.contains(r#"(allow file-read-metadata (literal "/m"))"#));
    }

    #[test]
    fn none_renders_deny_after_allows() {
        // A home rw with an .ssh hole; the deny must appear AFTER the rw allow.
        let p = Profile {
            paths: vec![
                grant("/home/u", Access::Rw, MatchKind::Subpath),
                grant("/home/u/.ssh", Access::None, MatchKind::Subpath),
            ],
            ..Default::default()
        };
        let pol = render_policy(&p);
        // Match the clause prefix (a symlink-resolved second target may follow);
        // the closing quote after `u` keeps this distinct from `/home/u/.ssh`.
        let allow_idx = pol
            .find(r#"(allow file-write* (subpath "/home/u")"#)
            .expect("rw allow present");
        let deny_idx = pol
            .find(r#"(deny file-read* file-write* (subpath "/home/u/.ssh")"#)
            .expect("none deny present");
        assert!(
            deny_idx > allow_idx,
            "deny must come after allow (last-match-wins)"
        );
        // Must never use the bare `file*` deny (it does not block writes).
        assert!(!pol.contains("(deny file* "));
    }

    #[test]
    fn ancestor_metadata_deduped() {
        let p = Profile {
            paths: vec![
                grant("/a/b/c", Access::Rw, MatchKind::Subpath),
                grant("/a/b/d", Access::Ro, MatchKind::Subpath),
            ],
            ..Default::default()
        };
        let pol = render_policy(&p);
        // Ancestors "/", "/a", "/a/b" each once.
        for anc in ["/", "/a", "/a/b"] {
            let line = format!(r#"(allow file-read-metadata (literal "{anc}"))"#);
            assert_eq!(
                pol.matches(&line).count(),
                1,
                "ancestor {anc} should appear exactly once"
            );
        }
        // The granted paths themselves are not ancestors.
        assert!(!pol.contains(r#"(allow file-read-metadata (literal "/a/b/c"))"#));
    }

    #[test]
    fn symlinked_path_emits_both_forms() {
        // macOS `/tmp` is a symlink to `/private/tmp`; Seatbelt does not treat the
        // two as equivalent, so a `/tmp` grant must emit BOTH forms or accesses via
        // the resolved path are wrongly denied. (Regression: field tests under
        // /var/folders were denied when only the resolved form was emitted.)
        let p = Profile {
            paths: vec![grant("/tmp", Access::Rw, MatchKind::Subpath)],
            ..Default::default()
        };
        let pol = render_policy(&p);
        assert!(pol.contains(r#"(subpath "/tmp")"#), "authored form present");
        assert!(
            pol.contains(r#"(subpath "/private/tmp")"#),
            "resolved form present"
        );
    }

    #[test]
    fn matcher_tokens() {
        let p = Profile {
            paths: vec![
                grant("/sub", Access::Ro, MatchKind::Subpath),
                grant("/lit", Access::Ro, MatchKind::Literal),
                grant("/pre", Access::Ro, MatchKind::Prefix),
                grant("/re.*", Access::Ro, MatchKind::Regex),
            ],
            ..Default::default()
        };
        let pol = render_policy(&p);
        assert!(pol.contains(r#"(subpath "/sub")"#));
        assert!(pol.contains(r#"(literal "/lit")"#));
        // prefix -> anchored, escaped regex
        assert!(pol.contains(r#"(regex "^/pre")"#));
        // regex -> verbatim pattern
        assert!(pol.contains(r#"(regex "/re.*")"#));
    }

    #[test]
    fn capabilities_render_and_pasteboard_corrected() {
        let p = Profile {
            macos: Some(MacosExtra {
                capabilities: vec![
                    Capability::MachLookup,
                    Capability::SysvSem,
                    Capability::Pasteboard,
                    Capability::ProcessInfo,
                ],
                raw: String::new(),
            }),
            ..Default::default()
        };
        let pol = render_policy(&p);
        assert!(pol.contains("(allow mach-lookup)"));
        assert!(pol.contains("(allow ipc-sysv-sem)"));
        assert!(pol.contains("(allow process-info*)"));
        // pasteboard corrected to the pboard mach service.
        assert!(pol.contains(r#"(allow mach-lookup (global-name "com.apple.pboard.service"))"#));
    }

    #[test]
    fn raw_appended_verbatim_last() {
        let p = Profile {
            paths: vec![grant("/x", Access::Ro, MatchKind::Subpath)],
            macos: Some(MacosExtra {
                capabilities: vec![],
                raw: "(allow mach-lookup (global-name \"com.example\"))".into(),
            }),
            ..Default::default()
        };
        let pol = render_policy(&p);
        assert!(pol
            .trim_end()
            .ends_with(r#"(allow mach-lookup (global-name "com.example"))"#));
    }

    #[test]
    fn string_escaping() {
        // A path with a quote and backslash must be escaped, not break the policy.
        let p = Profile {
            paths: vec![grant(r#"/weird/a"b\c"#, Access::Ro, MatchKind::Literal)],
            ..Default::default()
        };
        let pol = render_policy(&p);
        assert!(pol.contains(r#"(literal "/weird/a\"b\\c")"#));
    }
}
