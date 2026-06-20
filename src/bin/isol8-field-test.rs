//! isol8 field tests — ground-truth checks that the OS *actually* enforces the
//! policy. Each scenario builds an ad-hoc `Profile` + scratch workspace under the
//! temp dir, runs a probe command through the real backend, and asserts the
//! observed effect (see _docs/testing-strategies.md §3).
//!
//! Exit 0 if every scenario passes (skips allowed), 1 on any failure.
//!
//! ponytail: macOS-only enforcement for now (the only working backend); other
//! OSes print SKIP. Temp dirs removed on exit unless `--keep`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process;

use isol8::backends;
use isol8::env::build_minimal;
use isol8::profile::{Access, Capability, MacosExtra, MatchKind, PathGrant, Profile};

fn grant(path: &str, access: Access, m: MatchKind) -> PathGrant {
    PathGrant {
        path: path.to_string(),
        access,
        r#match: m,
    }
}

/// The minimal macOS allow-set any command needs to start under `(deny default)`,
/// verified on real `sandbox-exec` (see src/backends/macos.rs / profiles/macos-system.toml).
fn system_base() -> Vec<PathGrant> {
    use Access::Ro;
    use MatchKind::{Literal, Subpath};
    vec![
        grant("/usr", Ro, Subpath),
        grant("/bin", Ro, Subpath),
        grant("/System", Ro, Subpath),
        grant("/", Ro, Literal), // MANDATORY: cwd "/" inherited from launchd
        grant("/private/var/select", Ro, Subpath),
    ]
}

/// A profile with the macOS system base + scenario-specific grants.
fn profile_with(extra: Vec<PathGrant>) -> Profile {
    let mut paths = system_base();
    paths.extend(extra);
    Profile {
        requires: vec![],
        filter: None,
        policies: vec![],
        paths,
        env: HashMap::new(),
        home_replace: None,
        macos: Some(MacosExtra {
            capabilities: vec![Capability::ProcessExec, Capability::ProcessFork],
            raw: String::new(),
        }),
    }
}

/// Run a probe through the real sandbox; return its exit code.
fn run(profile: &Profile, home: &Path, cmd: &[&str]) -> i32 {
    let env = build_minimal(profile, home);
    let cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
    match backends::select().spawn(profile, &env, &cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("    spawn error: {e:#}");
            -1
        }
    }
}

struct Outcome {
    name: &'static str,
    pass: Option<bool>, // None = skip
}

fn main() {
    let keep = std::env::args().any(|a| a == "--keep");

    // Scenarios only enforce where a real backend exists. ponytail: macOS only.
    if !cfg!(target_os = "macos") {
        println!("isol8 field tests — no enforcing backend on this OS; all SKIP");
        return;
    }

    // One temp workspace for the whole run.
    let root = std::env::temp_dir().join(format!("isol8-ft-{}", process::id()));
    let home = root.join("home");
    let workspace = root.join("workspace");
    let seed = root.join("seed");
    let outside = root.join("outside");
    for d in [&home, &workspace, &seed, &outside] {
        fs::create_dir_all(d).expect("create temp dirs");
    }
    fs::write(seed.join("data.txt"), "seeded\n").unwrap();
    fs::write(outside.join("secret.txt"), "secret\n").unwrap();

    // A var that is NOT on the env allowlist — must not survive into the child.
    std::env::set_var("SECRET_TOKEN", "leak-me");

    let ws = workspace.to_str().unwrap();
    let sd = seed.to_str().unwrap();
    let out = outside.to_str().unwrap();
    let real_home = std::env::var("HOME").unwrap_or_else(|_| "/Users".into());

    println!(
        "isol8 field tests — backend: macos/seatbelt   home: {}\n",
        home.display()
    );

    let mut results = Vec::new();

    // 1. no grant on outside/ → read is Denied.
    {
        let p = profile_with(vec![]);
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", &format!("/bin/cat {out}/secret.txt")],
        );
        results.push(Outcome {
            name: "01 deny-read-outside-grant",
            pass: Some(code != 0),
        });
    }
    // 2. rw on workspace → write is Allowed.
    {
        let p = profile_with(vec![grant(ws, Access::Rw, MatchKind::Subpath)]);
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", &format!("echo hi > {ws}/out.txt")],
        );
        let wrote = workspace.join("out.txt").exists();
        results.push(Outcome {
            name: "02 rw-workspace-write",
            pass: Some(code == 0 && wrote),
        });
    }
    // 3. ro on seed → write is Denied.
    {
        let p = profile_with(vec![grant(sd, Access::Ro, MatchKind::Subpath)]);
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", &format!("echo hi > {sd}/x.txt")],
        );
        let blocked = !seed.join("x.txt").exists();
        results.push(Outcome {
            name: "03 ro-seed-write-denied",
            pass: Some(code != 0 && blocked),
        });
    }
    // 4. ro on seed → read is Allowed.
    {
        let p = profile_with(vec![grant(sd, Access::Ro, MatchKind::Subpath)]);
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", &format!("/bin/cat {sd}/data.txt")],
        );
        results.push(Outcome {
            name: "04 ro-seed-read",
            pass: Some(code == 0),
        });
    }
    // 5. scratch HOME → real home is unreadable.
    {
        let p = profile_with(vec![]); // real home not granted
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", &format!("/bin/ls {real_home}")],
        );
        results.push(Outcome {
            name: "05 real-home-denied",
            pass: Some(code != 0),
        });
    }
    // 6. env allowlist → non-allowlisted var is absent (printenv exits 1).
    {
        let p = profile_with(vec![]);
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", "/usr/bin/printenv SECRET_TOKEN"],
        );
        results.push(Outcome {
            name: "06 env-secret-absent",
            pass: Some(code != 0),
        });
    }
    // 7. env allowlist → PATH and HOME are present (printenv exits 0).
    {
        let p = profile_with(vec![]);
        let code = run(
            &p,
            &home,
            &[
                "/bin/sh",
                "-c",
                "/usr/bin/printenv HOME && /usr/bin/printenv PATH",
            ],
        );
        results.push(Outcome {
            name: "07 env-path-home-present",
            pass: Some(code == 0),
        });
    }
    // 8. network — not implemented.
    results.push(Outcome {
        name: "08 net-n0-deny",
        pass: None,
    });

    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    for r in &results {
        let tag = match r.pass {
            Some(true) => {
                passed += 1;
                "PASS"
            }
            Some(false) => {
                failed += 1;
                "FAIL"
            }
            None => {
                skipped += 1;
                "SKIP"
            }
        };
        let note = if r.pass.is_none() {
            "   (network tier not implemented)"
        } else {
            ""
        };
        println!("  {tag}  {}{note}", r.name);
    }
    println!("\n  {passed} passed, {failed} failed, {skipped} skipped");

    if keep {
        println!("  --keep: left {} in place", root.display());
    } else {
        let _ = fs::remove_dir_all(&root);
    }

    process::exit(if failed == 0 { 0 } else { 1 });
}
