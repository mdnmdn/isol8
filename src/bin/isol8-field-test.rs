//! isol8 field tests — ground-truth checks that the OS *actually* enforces the
//! policy. Each scenario builds an ad-hoc `Profile` + scratch workspace under the
//! temp dir, runs a probe command through the real backend, and asserts the
//! observed effect (see _docs/testing-strategies.md §3).
//!
//! macOS (Seatbelt) and Linux (Landlock) enforce all path scenarios. Windows enforces
//! path scenarios when `isol8-winhook.dll` is present (hybrid AppContainer + hook;
//! see `_docs/inbox/windows-policy-approach.md`). Without the hook DLL, Windows path
//! scenarios SKIP. Linux-specific scenarios (10–16) compile in only on Linux.
//!
//! Exit 0 if every scenario passes (skips allowed), 1 on any failure.
//! Temp dirs removed on exit unless `--keep`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process;

use isol8::backends;
use isol8::env::build_minimal;
use isol8::profile::{
    apply_rewrite, Access, Capability, MacosExtra, MatchKind, PathGrant, Profile, Rewrite,
};
use isol8::resolve::confine_executable;

fn grant(path: &str, access: Access, m: MatchKind) -> PathGrant {
    PathGrant {
        path: path.to_string(),
        access,
        r#match: m,
    }
}

/// Minimal system-base grants needed for the process to start, per platform.
///
/// macOS: verified on real `sandbox-exec` (see profiles/macos-system.toml).
/// Linux: the paths a child needs under Landlock deny-by-default. NOTE: /tmp is
/// deliberately NOT granted — test dirs live under /tmp and must be outside any
/// base grant so deny-by-default blocks them.
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
        "linux" => {
            use Access::Ro;
            use MatchKind::Subpath;
            vec![
                grant("/usr", Ro, Subpath),
                grant("/bin", Ro, Subpath),
                grant("/lib", Ro, Subpath),
                grant("/lib64", Ro, Subpath),
                grant("/etc", Ro, Subpath),
                grant("/dev", Ro, Subpath),
                grant("/proc", Ro, Subpath),
            ]
        }
        "windows" => {
            use Access::Ro;
            use MatchKind::Subpath;
            let sysroot = std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".to_string());
            // Do NOT grant all of %TEMP% — outside/ is a sibling under the temp parent and
            // must stay outside any base grant (same invariant as Linux field tests).
            vec![grant(&sysroot, Ro, Subpath)]
        }
        _ => vec![],
    }
}

fn probe_exe() -> String {
    let path = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("exe parent dir")
        .join("isol8-probe.exe");
    assert!(
        path.is_file(),
        "isol8-probe.exe not found at {}; run `cargo build --bin isol8-probe`",
        path.display()
    );
    path.to_string_lossy().into_owned()
}

/// Platform-specific probe command fragments for path scenarios.
fn probe_read(path: &str) -> Vec<String> {
    if std::env::consts::OS == "windows" {
        vec![probe_exe(), "read".into(), path.into()]
    } else {
        vec!["/bin/sh".into(), "-c".into(), format!("/bin/cat {path}")]
    }
}

fn probe_write(path: &str) -> Vec<String> {
    if std::env::consts::OS == "windows" {
        vec![probe_exe(), "write".into(), path.into()]
    } else {
        vec!["/bin/sh".into(), "-c".into(), format!("echo hi > {path}")]
    }
}

fn probe_list_dir(path: &str) -> Vec<String> {
    if std::env::consts::OS == "windows" {
        let marker = format!("{path}\\.isol8-ft-probe");
        vec![probe_exe(), "read".into(), marker]
    } else {
        vec!["/bin/sh".into(), "-c".into(), format!("/bin/ls {path}")]
    }
}

/// A profile with the platform system base + scenario-specific grants.
fn profile_with(extra: Vec<PathGrant>, root: &Path) -> Profile {
    let mut paths = system_base();
    if std::env::consts::OS == "windows" {
        // Grant only this run's scratch tree, not all of %TEMP%.
        paths.push(grant(
            root.to_str().expect("scratch root is valid UTF-8"),
            Access::Rw,
            MatchKind::Subpath,
        ));
    }
    paths.extend(extra);
    let (macos, windows) = match std::env::consts::OS {
        "macos" => (
            Some(MacosExtra {
                capabilities: vec![Capability::ProcessExec, Capability::ProcessFork],
                raw: String::new(),
            }),
            None,
        ),
        // Hook mode (Tier 1b) does not use AppContainer; skip windows capabilities here.
        "windows" => (None, None),
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
    let mut profile = profile.clone();
    let mut cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
    if let Err(e) = confine_executable(&mut profile, &mut cmd) {
        eprintln!("    confine_executable error: {e:#}");
        return -1;
    }
    let env = build_minimal(&profile, home, &[], &[]);
    match backends::select()
        .spawn(&profile, &env, &cmd)
        .and_then(|mut child| child.wait())
    {
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
    note: &'static str,
}

fn main() {
    let keep = std::env::args().any(|a| a == "--keep");
    let platform = std::env::consts::OS;

    // One temp workspace for the whole run.
    let root = std::env::temp_dir().join(format!("isol8-ft-{}", process::id()));
    let home = root.join("home");
    let workspace = root.join("workspace");
    let seed = root.join("seed");
    // outside/ must be OUTSIDE any granted path. On Linux, /tmp is granted rw
    // by the system base, so place outside/ as a sibling of root (still under
    // /tmp but NOT under root). The sandbox cannot reach it because no
    // PathBeneath rule covers it — Landlock deny-by-default blocks access.
    let outside = root.parent().unwrap_or(&root).join(format!(
        "outside-{}",
        root.file_name().unwrap().to_string_lossy()
    ));
    for d in [&home, &workspace, &seed] {
        fs::create_dir_all(d).expect("create temp dirs");
    }
    fs::create_dir_all(&outside).expect("create outside dir");
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
    if platform == "windows" {
        let marker = std::path::Path::new(&real_home).join(".isol8-ft-probe");
        let _ = fs::write(&marker, "probe\n");
    }

    println!(
        "isol8 field tests — platform: {platform}   home: {}\n",
        home.display()
    );

    let mut results = Vec::new();

    // Path scenarios need OS-level enforcement. macOS/Linux use Seatbelt/Landlock;
    // Windows uses the hybrid hook DLL when deployed beside the binary.
    let path_enforced = matches!(platform, "macos" | "linux")
        || (platform == "windows" && backends::path_enforcement_available());

    // ===== Cross-platform scenarios (1–9) =====

    // 1. no grant on outside/ → read is Denied.
    {
        let p = profile_with(vec![], &root);
        let (code, note) = if path_enforced {
            let secret = format!("{out}\\secret.txt");
            let argv = probe_read(&secret);
            let cmd: Vec<&str> = argv.iter().map(String::as_str).collect();
            let c = run(&p, &home, &cmd);
            (c, "")
        } else {
            (
                -1,
                if platform == "windows" {
                    "isol8-winhook.dll not found beside binary"
                } else {
                    "path enforcement not available on this platform"
                },
            )
        };
        results.push(Outcome {
            name: "01 deny-read-outside-grant",
            pass: if path_enforced { Some(code != 0) } else { None },
            note,
        });
    }
    // 2. rw on workspace → write is Allowed.
    {
        let p = profile_with(vec![grant(ws, Access::Rw, MatchKind::Subpath)], &root);
        let (code, wrote, note) = if path_enforced {
            let out_file = if platform == "windows" {
                format!("{ws}\\out.txt")
            } else {
                format!("{ws}/out.txt")
            };
            let argv = probe_write(&out_file);
            let cmd: Vec<&str> = argv.iter().map(String::as_str).collect();
            let c = run(&p, &home, &cmd);
            (c, workspace.join("out.txt").exists(), "")
        } else {
            (
                -1,
                false,
                if platform == "windows" {
                    "isol8-winhook.dll not found beside binary"
                } else {
                    "path enforcement not available on this platform"
                },
            )
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
        let p = profile_with(vec![grant(sd, Access::Ro, MatchKind::Subpath)], &root);
        let (code, blocked, note) = if path_enforced {
            let target = if platform == "windows" {
                format!("{sd}\\x.txt")
            } else {
                format!("{sd}/x.txt")
            };
            let argv = probe_write(&target);
            let cmd: Vec<&str> = argv.iter().map(String::as_str).collect();
            let c = run(&p, &home, &cmd);
            (c, !seed.join("x.txt").exists(), "")
        } else {
            (
                -1,
                false,
                if platform == "windows" {
                    "isol8-winhook.dll not found beside binary"
                } else {
                    "path enforcement not available on this platform"
                },
            )
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
        let p = profile_with(vec![grant(sd, Access::Ro, MatchKind::Subpath)], &root);
        let (code, note) = if path_enforced {
            let target = format!("{sd}/data.txt");
            let argv = probe_read(&target);
            let cmd: Vec<&str> = argv.iter().map(String::as_str).collect();
            let c = run(&p, &home, &cmd);
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
        let p = profile_with(vec![], &root);
        let (code, note) = if path_enforced {
            let argv = probe_list_dir(&real_home);
            let cmd: Vec<&str> = argv.iter().map(String::as_str).collect();
            let c = run(&p, &home, &cmd);
            (c, "")
        } else {
            (
                -1,
                if platform == "windows" {
                    "isol8-winhook.dll not found beside binary"
                } else {
                    "path enforcement not available on this platform"
                },
            )
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
            let p = profile_with(vec![], &root);
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
            let p = profile_with(vec![], &root);
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
            let p = profile_with(vec![], &root);
            let c = run(
                &p,
                &home,
                &["cmd.exe", "/c", "if defined HOME (exit 0) else (exit 1)"],
            );
            (c, "")
        } else {
            let p = profile_with(vec![], &root);
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
    // 9. command rewrite (Unix) or AppContainer spawn smoke test (Windows).
    {
        let (code, injected_made, note) = if platform == "windows" {
            let p = profile_with(vec![], &root);
            let c = run(&p, &home, &["cmd.exe", "/c", "exit 0"]);
            (c, c == 0, "AppContainer CreateProcessW smoke test")
        } else if path_enforced {
            let mut p = profile_with(vec![grant(ws, Access::Rw, MatchKind::Subpath)], &root);
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
            (-1, false, "rewrite field test only on Unix backends")
        };
        results.push(Outcome {
            name: if platform == "windows" {
                "09 appcontainer-spawn"
            } else {
                "09 rewrite-injects-arg"
            },
            pass: if platform == "windows" || path_enforced {
                Some(code == 0 && injected_made)
            } else {
                None
            },
            note,
        });
    }

    // 10. grandchild subprocess inherits hook policy (Windows hook mode only).
    #[cfg(target_os = "windows")]
    {
        let (code, note) = if path_enforced {
            let secret = format!("{out}\\secret.txt");
            let argv = [
                probe_exe(),
                "spawn".into(),
                "read".into(),
                secret,
            ];
            let p = profile_with(vec![], &root);
            let cmd: Vec<&str> = argv.iter().map(String::as_str).collect();
            let c = run(&p, &home, &cmd);
            (c, "grandchild read outside grant must fail")
        } else {
            (
                -1,
                "isol8-winhook.dll not found beside binary",
            )
        };
        results.push(Outcome {
            name: "10 grandchild-deny-outside-grant",
            pass: if path_enforced { Some(code != 0) } else { None },
            note,
        });
    }

    // ===== Linux-specific scenarios (10–16) =====

    // 10. no grant on outside/ → read is Denied (Landlock enforcement check).
    //     Same as scenario 1 but exercises Landlock deny-by-default on a
    //     path that Unix DAC would allow (world-readable temp dir).
    #[cfg(target_os = "linux")]
    {
        let p = profile_with(vec![], &root);
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", &format!("/bin/cat {out}/secret.txt")],
        );
        results.push(Outcome {
            name: "10 linux-deny-ungranted-path",
            pass: Some(code != 0),
            note: "",
        });
    }

    // 11. rw on workspace → write succeeds (Landlock rw enforcement).
    #[cfg(target_os = "linux")]
    {
        let p = profile_with(vec![grant(ws, Access::Rw, MatchKind::Subpath)], &root);
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", &format!("echo test > {ws}/linux-test.txt")],
        );
        let wrote = workspace.join("linux-test.txt").exists();
        results.push(Outcome {
            name: "11 linux-rw-write-allowed",
            pass: Some(code == 0 && wrote),
            note: "",
        });
    }

    // 12. ro on seed → write is Denied (Landlock ro enforcement).
    #[cfg(target_os = "linux")]
    {
        let p = profile_with(vec![grant(sd, Access::Ro, MatchKind::Subpath)], &root);
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", &format!("echo hack > {sd}/linux-x.txt")],
        );
        let blocked = !seed.join("linux-x.txt").exists();
        results.push(Outcome {
            name: "12 linux-ro-write-denied",
            pass: Some(code != 0 && blocked),
            note: "",
        });
    }

    // 13. ro on seed → read is Allowed.
    #[cfg(target_os = "linux")]
    {
        let p = profile_with(vec![grant(sd, Access::Ro, MatchKind::Subpath)], &root);
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", &format!("/bin/cat {sd}/data.txt")],
        );
        results.push(Outcome {
            name: "13 linux-ro-read-allowed",
            pass: Some(code == 0),
            note: "",
        });
    }

    // 14. no grant on real home → ls is Denied (no ancestor over-granting).
    //     Before the fix, metadata ancestor rules would expose the real home.
    #[cfg(target_os = "linux")]
    {
        let p = profile_with(vec![], &root); // no home grant
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", &format!("/bin/ls {real_home}")],
        );
        results.push(Outcome {
            name: "14 linux-real-home-denied",
            pass: Some(code != 0),
            note: "",
        });
    }

    // 15. env allowlist → SECRET_TOKEN is absent.
    #[cfg(target_os = "linux")]
    {
        let p = profile_with(vec![], &root);
        let code = run(
            &p,
            &home,
            &["/bin/sh", "-c", "/usr/bin/printenv SECRET_TOKEN"],
        );
        results.push(Outcome {
            name: "15 linux-env-secret-absent",
            pass: Some(code != 0),
            note: "",
        });
    }

    // 16. env allowlist → PATH and HOME are present.
    #[cfg(target_os = "linux")]
    {
        let p = profile_with(vec![], &root);
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
            name: "16 linux-env-path-home-present",
            pass: Some(code == 0),
            note: "",
        });
    }

    // ===== Report =====

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
        let note = if r.note.is_empty() {
            String::new()
        } else {
            format!("   ({})", r.note)
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
