//! `@diag` — diagnose why a confined command aborts at launch (SIGABRT / exit 134).
//!
//! A deny-by-default sandbox aborts a process when it denies a path the runtime needs
//! to *start* (the dyld shared cache, `/`, a dylib dir, …). The failure is a bare
//! SIGABRT with no diagnostic, indistinguishable from a real crash. `@diag` finds the
//! culprit automatically:
//!
//! 1. Render the command's real effective policy (the one that aborts).
//! 2. Confirm the command *launches* once read access to every top-level directory is
//!    added — i.e. it really is a missing path grant, not a capability/network issue.
//! 3. **Dichotomically minimize** (delta-debug) that added set, re-running the command
//!    under each trial policy, until only the grants whose absence causes the abort
//!    remain. SIGABRT (signal death) is the failure signal; a real exit code = launched.
//!
//! macOS-only: it drives the real `sandbox-exec`, the only enforcing backend today.

use anyhow::Result;

use crate::cli::RunArgs;

#[cfg(target_os = "macos")]
pub fn run(args: &RunArgs) -> Result<()> {
    macos::run(args)
}

#[cfg(not(target_os = "macos"))]
pub fn run(_args: &RunArgs) -> Result<()> {
    anyhow::bail!("@diag is only supported on macOS (the only enforcing backend so far)");
}

#[cfg(target_os = "macos")]
mod macos {
    use std::collections::HashMap;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};

    use crate::backends::macos::render_policy;
    use crate::cli::RunArgs;
    use crate::resolve;

    const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
    /// Per-trial launch budget. `@diag` is meant for fast-exiting probes (`node
    /// --version`); a long-running command is killed and counted as "launched".
    const TRIAL_TIMEOUT: Duration = Duration::from_secs(10);

    pub fn run(args: &RunArgs) -> Result<()> {
        let mut eff = resolve::effective_policy(args)?;
        if eff.cmd.is_empty() {
            bail!("@diag needs a command (e.g. isol8 @diag node --version)");
        }
        crate::home::seed(&eff.home)?;
        resolve::confine_executable(&mut eff.profile, &mut eff.cmd)?;

        let base = render_policy(&eff.profile);
        let env = &eff.env;
        let cmd = &eff.cmd;
        let pretty = cmd.join(" ");

        println!("== isol8 @diag: {pretty} ==\n");

        // Does it already launch? Then there is nothing to diagnose.
        if launches(&base, env, cmd)? {
            println!("'{pretty}' already launches under the current policy — nothing to diagnose.");
            println!("(A non-zero *exit code* is the command's own business; @diag only chases launch aborts.)");
            return Ok(());
        }
        println!("'{pretty}' is aborted at launch by the current sandbox policy. Searching for the missing grant…\n");

        let roots = root_candidates();

        // Read-only first (the common case); fall back to full read+write if reads alone
        // can't get it to launch.
        let (candidates, mode) = {
            let reads = candidate_rules(&roots, false);
            if launches(&with(&base, &reads), env, cmd)? {
                (reads, "read")
            } else {
                let full = candidate_rules(&roots, true);
                if !launches(&with(&base, &full), env, cmd)? {
                    bail!(
                        "could not make '{pretty}' launch even granting read+write to every \
                         top-level directory. The cause is likely a capability, network, or \
                         device requirement rather than a path grant. Inspect the policy with \
                         `isol8 --show-policies {pretty}`."
                    );
                }
                (full, "read+write")
            }
        };

        // Dichotomic minimization: shrink `candidates` to the set whose absence aborts.
        let trials = std::cell::Cell::new(0usize);
        let probe = |rules: &[String]| -> Result<bool> {
            trials.set(trials.get() + 1);
            launches(&with(&base, rules), env, cmd)
        };
        let needed = ddmin(&base, Vec::new(), candidates, env, cmd, &probe)?;

        report(&pretty, mode, &needed, trials.get());
        Ok(())
    }

    /// Top-level directories of `/` (each a `file-read*`/`file*` subpath candidate).
    fn root_candidates() -> Vec<String> {
        let mut roots: Vec<String> = match std::fs::read_dir("/") {
            Ok(rd) => rd
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .map(|n| format!("/{n}"))
                .collect(),
            Err(_) => Vec::new(),
        };
        roots.sort();
        roots.dedup();
        roots
    }

    fn candidate_rules(roots: &[String], full: bool) -> Vec<String> {
        let op = if full { "file*" } else { "file-read*" };
        // The root directory `/` itself is read by every process at launch (inherited
        // cwd from launchd) and is NOT covered by any `(subpath "/child")`, so it must
        // be a candidate in its own right.
        let mut rules = vec![format!("(allow {op} (literal \"/\"))")];
        rules.extend(
            roots
                .iter()
                .map(|r| format!("(allow {op} (subpath {:?}))", r)),
        );
        rules
    }

    fn with(base: &str, rules: &[String]) -> String {
        let mut s = String::with_capacity(base.len() + rules.len() * 48 + 1);
        s.push_str(base);
        if !base.ends_with('\n') {
            s.push('\n');
        }
        for r in rules {
            s.push_str(r);
            s.push('\n');
        }
        s
    }

    /// Delta-debugging to a 1-minimal subset of `rest` (on top of `keep`) that still
    /// launches. Relies on monotonicity: more grants never break a launch.
    ///
    /// Precondition: `probe(keep ∪ rest)` launches. Returns the minimal additions.
    fn ddmin(
        base: &str,
        keep: Vec<String>,
        rest: Vec<String>,
        env: &HashMap<String, String>,
        cmd: &[String],
        probe: &dyn Fn(&[String]) -> Result<bool>,
    ) -> Result<Vec<String>> {
        let _ = (base, env, cmd); // probe closes over them
        if rest.len() <= 1 {
            // A lone element is necessary iff `keep` alone fails to launch.
            return if probe(&keep)? {
                Ok(Vec::new())
            } else {
                Ok(rest)
            };
        }
        let mid = rest.len() / 2;
        let a = rest[..mid].to_vec();
        let b = rest[mid..].to_vec();

        let keep_a: Vec<String> = keep.iter().cloned().chain(a.iter().cloned()).collect();
        if probe(&keep_a)? {
            return ddmin(base, keep, a, env, cmd, probe); // b unneeded
        }
        let keep_b: Vec<String> = keep.iter().cloned().chain(b.iter().cloned()).collect();
        if probe(&keep_b)? {
            return ddmin(base, keep, b, env, cmd, probe); // a unneeded
        }
        // Interference: both halves contribute. Resolve a with b present, then b.
        let with_b: Vec<String> = keep.iter().cloned().chain(b.iter().cloned()).collect();
        let need_a = ddmin(base, with_b, a, env, cmd, probe)?;
        let keep2: Vec<String> = keep.iter().cloned().chain(need_a.iter().cloned()).collect();
        let need_b = ddmin(base, keep2, b, env, cmd, probe)?;
        Ok(need_a.into_iter().chain(need_b).collect())
    }

    /// Run `cmd` under `policy`; return true if it *launched* (got a real exit code),
    /// false if the sandbox killed it at start (signal / SIGABRT) or it timed out.
    fn launches(policy: &str, env: &HashMap<String, String>, cmd: &[String]) -> Result<bool> {
        let mut command = Command::new(SANDBOX_EXEC);
        command.arg("-p").arg(policy).args(cmd);
        command.env_clear().envs(env);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .with_context(|| format!("launching {SANDBOX_EXEC}"))?;

        let start = Instant::now();
        loop {
            match child.try_wait()? {
                // A real exit code (even non-zero) means the process actually launched —
                // except sandbox-exec's own exit 71, which means it could not *exec* the
                // command at all (e.g. the binary's own directory isn't readable). Treat
                // that as "not launched" so @diag can hunt the grant that fixes it too.
                Some(status) => return Ok(matches!(status.code(), Some(c) if c != 71)),
                None => {
                    if start.elapsed() > TRIAL_TIMEOUT {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(true); // ran long enough to count as launched
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
            }
        }
    }

    fn report(pretty: &str, mode: &str, needed: &[String], trials: usize) {
        if needed.is_empty() {
            println!("No additional grant was required in isolation — the abort may be order- or interaction-dependent. Inspect with `isol8 --show-policies {pretty}`.");
            return;
        }
        let dirs: Vec<String> = needed.iter().filter_map(|r| path_of(r)).collect();
        let access = if mode == "read" { "ro" } else { "rw" };
        println!("Found it in {trials} trials. '{pretty}' launches once the sandbox grants {mode} access to:\n");
        for d in &dirs {
            println!("  {d}");
        }

        // The root `/` must be granted LITERAL (the directory node only) — `--add-dirs-*`
        // emits a subpath grant, which on `/` would open the whole filesystem, so suggest
        // the profile literal form instead.
        let flag = if mode == "read" {
            "--add-dirs-ro"
        } else {
            "--add-dirs-rw"
        };
        let cli_dirs: Vec<&String> = dirs.iter().filter(|d| d.as_str() != "/").collect();
        if !cli_dirs.is_empty() {
            let adds: Vec<String> = cli_dirs.iter().map(|d| format!("{flag} {d}")).collect();
            println!("\nFix — grant it for this run:");
            println!("  isol8 {} -- {pretty}", adds.join(" "));
        }
        println!("\nor add to a profile layer:");
        for d in &dirs {
            if d == "/" {
                println!("  {{ path = \"/\", access = {access:?}, match = \"literal\" }}");
            } else {
                println!("  {{ path = {d:?}, access = {access:?} }}");
            }
        }
    }

    /// Pull `X` out of `(allow … (subpath "X"))` or `(allow … (literal "X"))`.
    fn path_of(rule: &str) -> Option<String> {
        let anchor = rule.find("(subpath ").or_else(|| rule.find("(literal "))?;
        let rest = &rule[anchor..];
        let start = rest.find('"')? + 1;
        let end = rest[start..].find('"')? + start;
        Some(rest[start..end].to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn path_extraction() {
            assert_eq!(
                path_of("(allow file-read* (subpath \"/usr/lib\"))").as_deref(),
                Some("/usr/lib")
            );
            assert_eq!(
                path_of("(allow file-read* (literal \"/\"))").as_deref(),
                Some("/")
            );
            assert_eq!(path_of("(allow process-exec)"), None);
        }

        #[test]
        fn candidate_rules_quote_paths() {
            // The root `/` literal is always prepended, then one subpath rule per root.
            let r = candidate_rules(&["/usr".into()], false);
            assert_eq!(
                r,
                vec![
                    "(allow file-read* (literal \"/\"))".to_string(),
                    "(allow file-read* (subpath \"/usr\"))".to_string(),
                ]
            );
            let w = candidate_rules(&["/x".into()], true);
            assert_eq!(
                w,
                vec![
                    "(allow file* (literal \"/\"))".to_string(),
                    "(allow file* (subpath \"/x\"))".to_string(),
                ]
            );
        }

        // ddmin over a synthetic monotone oracle: "launches" iff the kept set contains
        // the two required rules. Confirms minimization isolates exactly those.
        #[test]
        fn ddmin_isolates_required() {
            let all: Vec<String> = (0..8).map(|i| format!("r{i}")).collect();
            let required = ["r3".to_string(), "r6".to_string()];
            let probe = |rules: &[String]| -> Result<bool> {
                Ok(required.iter().all(|req| rules.iter().any(|r| r == req)))
            };
            let env = HashMap::new();
            let got = ddmin("", Vec::new(), all, &env, &[], &probe).unwrap();
            let mut got = got;
            got.sort();
            assert_eq!(got, vec!["r3".to_string(), "r6".to_string()]);
        }
    }
}
