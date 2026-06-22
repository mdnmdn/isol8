//! isol8 field tests — ground-truth checks that the OS *actually* enforces the
//! policy. Each scenario builds an ad-hoc `Profile` + scratch workspace under the
//! temp dir, runs a probe command through the real backend, and asserts the
//! observed effect (see _docs/testing-strategies.md §3).
//!
//! macOS enforces all scenarios. Windows runs env/rewrite scenarios; path
//! scenarios SKIP (ACL-level enforcement deferred to Phase 5). Linux SKIP all
//! (no Landlock backend yet).
//!
//! Exit 0 if every scenario passes (skips allowed), 1 on any failure.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process;

use isol8::backends;
use isol8::env::build_minimal;
use isol8::profile::{
    apply_rewrite, Access, Capability, MacosExtra, MatchKind, PathGrant, Profile, Rewrite,
    WindowsCapability, WindowsExtra,
};

fn grant(path: &str, access: Access, m: MatchKind) -> PathGrant {
    PathGrant {
        path: path.to_string(),
        access,
        r#match: m,
    }
}

/// Minimal system-base grants needed for the process to start.
fn system_base() -> Vec<PathGrant> {
    match std::env::consts::OS {
        "macos" => {
            use Access::Ro;
            use MatchKind::{Literal, Subpath};
            vec![
                grant("/usr", Ro, Subpath),
                grant("/bin", Ro, Subpath),
                grant("/System", Ro, Subpath),
                grant("/", Ro, Literal),
                grant("/private/var/select", Ro, Subpath),
            ]
        }
        _ => vec![],
    }
}

fn profile_with(extra: Vec<PathGrant>) -> Profile {
    let mut paths = system_base();
    paths.extend(extra);
    let (macos, windows) = match std::env::consts::OS {
        "macos" => (
            Some(MacosExtra {
                capabilities: vec![Capability::ProcessExec, Capability::ProcessFork],
                raw: String::new(),
            }),
            None,
        ),
        "windows" => (
            None,
            Some(WindowsExtra {
                capabilities: vec![WindowsCapability::InternetClient],
            }),
        ),
        _ => (None, None),
    };
    Profile {
        requires: vec![],
        filter: None,
        policies: vec![],
        paths,
        env: HashMap::new(),
        home_replace: None,
        rewrite: None,
        macos,
        windows,
    }
}

fn run(profile: &Profile, home: &Path, cmd: &[&str]) -> i32 {
    let env = build_minimal(profile, home, &[], &[]);
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
    pass: Option<bool>,
    note: &'static str,
}

fn main() {
    let keep = std::env::args().any(|a| a == "--keep");
    let platform = std::env::consts::OS;

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

    std::env::set_var("SECRET_TOKEN", "leak-me");

    let ws = workspace.to_str().unwrap();
    let sd = seed.to_str().unwrap();
    let out = outside.to_str().unwrap();
    let real_home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| {
            if platform == "windows" {
                "C:\\Users".into()
            } else {
                "/Users".into()
            }
        });

    println!(
        "isol8 field tests — platform: {platform}   home: {}",
        home.display()
    );

    let mut results = Vec::new();

    // Path scenarios need ACL-level enforcement. macOS (Seatbelt) does it;
    // Windows AppContainer needs ACL modification (Phase 5).
    let path_enforced = platform == "macos";

    // 1. no grant on outside/ → read is Denied.
    {
        let p = profile_with(vec![]);
        let (code, note) = if path_enforced {
            let c = run(
                &p,
                &home,
                &["/bin/sh", "-c", &format!("/bin/cat {out}/secret.txt")],
            );
            (c, "")
        } else {
            (-1, "path enforcement not available on this platform")
        };
        results.push(Outcome {
            name: "01 deny-read-outside-grant",
            pass: if path_enforced { Some(code != 0) } else { None },
            note,
        });
    }
    // 2. rw on workspace → write is Allowed.
    {
        let p = profile_with(vec![grant(ws, Access::Rw, MatchKind::Subpath)]);
        let (code, wrote, note) = if path_enforced {
            let c = run(
                &p,
                &home,
                &["/bin/sh", "-c", &format!("echo hi > {ws}/out.txt")],
            );
            (c, workspace.join("out.txt").exists(), "")
        } else {
            (-1, false, "path enforcement not available on this platform")
        };
        results.push(Outcome {
            name: "02 rw-workspace-write",
            pass: if path_enforced {
                Some(code == 0 && wrote)
            } else {
                None
            },
            note,
        });
    }
    // 3. ro on seed → write is Denied.
    {
        let p = profile_with(vec![grant(sd, Access::Ro, MatchKind::Subpath)]);
        let (code, blocked, note) = if path_enforced {
            let c = run(
                &p,
                &home,
                &["/bin/sh", "-c", &format!("echo hi > {sd}/x.txt")],
            );
            (c, !seed.join("x.txt").exists(), "")
        } else {
            (-1, false, "path enforcement not available on this platform")
        };
        results.push(Outcome {
            name: "03 ro-seed-write-denied",
            pass: if path_enforced {
                Some(code != 0 && blocked)
            } else {
                None
            },
            note,
        });
    }
    // 4. ro on seed → read is Allowed.
    {
        let p = profile_with(vec![grant(sd, Access::Ro, MatchKind::Subpath)]);
        let (code, note) = if path_enforced {
            let c = run(
                &p,
                &home,
                &["/bin/sh", "-c", &format!("/bin/cat {sd}/data.txt")],
            );
            (c, "")
        } else {
            (-1, "path enforcement not available on this platform")
        };
        results.push(Outcome {
            name: "04 ro-seed-read",
            pass: if path_enforced { Some(code == 0) } else { None },
            note,
        });
    }
    // 5. scratch HOME → real home is unreadable.
    {
        let p = profile_with(vec![]);
        let (code, note) = if path_enforced {
            let c = run(
                &p,
                &home,
                &["/bin/sh", "-c", &format!("/bin/ls {real_home}")],
            );
            (c, "")
        } else {
            (-1, "path enforcement not available on this platform")
        };
        results.push(Outcome {
            name: "05 real-home-denied",
            pass: if path_enforced { Some(code != 0) } else { None },
            note,
        });
    }
    // 6. env allowlist → non-allowlisted var is absent.
    {
        let (code, note) = if platform == "windows" {
            let p = profile_with(vec![]);
            let code = run(
                &p,
                &home,
                &[
                    "cmd.exe",
                    "/c",
                    "if defined SECRET_TOKEN (exit 0) else (exit 1)",
                ],
            );
            (code, "")
        } else {
            let p = profile_with(vec![]);
            let c = run(
                &p,
                &home,
                &["/bin/sh", "-c", "/usr/bin/printenv SECRET_TOKEN"],
            );
            (c, "")
        };
        results.push(Outcome {
            name: "06 env-secret-absent",
            pass: Some(code != 0),
            note,
        });
    }
    // 7. env allowlist → PATH and HOME are present.
    {
        let (code, note) = if platform == "windows" {
            let p = profile_with(vec![]);
            let c = run(
                &p,
                &home,
                &["cmd.exe", "/c", "if defined HOME (exit 0) else (exit 1)"],
            );
            (c, "")
        } else {
            let p = profile_with(vec![]);
            let c = run(
                &p,
                &home,
                &[
                    "/bin/sh",
                    "-c",
                    "/usr/bin/printenv HOME && /usr/bin/printenv PATH",
                ],
            );
            (c, "")
        };
        results.push(Outcome {
            name: "07 env-path-home-present",
            pass: Some(code == 0),
            note,
        });
    }
    // 8. network — not implemented on any platform.
    results.push(Outcome {
        name: "08 net-n0-deny",
        pass: None,
        note: "network tier not implemented",
    });
    // 9. command rewrite → an injected arg reaches the executed program.
    // macOS only: /usr/bin/touch works cleanly with apply_rewrite; Windows
    // cmd.exe /c is incompatible with the arg-rewrite model, and path
    // enforcement isn't available anyway.
    {
        let (code, injected_made, note) = if platform == "macos" {
            let mut p = profile_with(vec![grant(ws, Access::Rw, MatchKind::Subpath)]);
            let injected = format!("{ws}/injected.txt");
            p.rewrite = Some(Rewrite {
                ensure_args: vec![injected.clone()],
            });
            let base = vec!["/usr/bin/touch".to_string(), format!("{ws}/base.txt")];
            let rewritten = apply_rewrite(&base, &p.rewrite);
            let argv: Vec<&str> = rewritten.iter().map(String::as_str).collect();
            let c = run(&p, &home, &argv);
            (c, workspace.join("injected.txt").exists(), "")
        } else {
            (-1, false, "rewrite field test only on macOS")
        };
        results.push(Outcome {
            name: "09 rewrite-injects-arg",
            pass: if platform == "macos" {
                Some(code == 0 && injected_made)
            } else {
                None
            },
            note,
        });
    }

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
        let note = if r.note.is_empty() && r.pass.is_none() {
            "   (network tier not implemented)"
        } else if !r.note.is_empty() {
            &format!("   ({})", r.note)
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
