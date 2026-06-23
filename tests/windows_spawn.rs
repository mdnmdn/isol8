//! Windows hook spawn integration tests (require isol8-winhook.dll beside test binary).

#![cfg(windows)]

use std::fs;

use isol8::backends;
use isol8::env::build_minimal;
use isol8::profile::{Access, MatchKind, PathGrant, Profile};
use isol8::resolve::confine_executable;

fn grant(path: &str, access: Access, m: MatchKind) -> PathGrant {
    PathGrant {
        path: path.to_string(),
        access,
        r#match: m,
    }
}

fn probe_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // Unit-test binaries live in target/debug/deps; binaries are in target/debug.
    let debug = exe.parent()?.parent()?;
    let probe = debug.join("isol8-probe.exe");
    probe.is_file().then_some(probe)
}

#[test]
fn ro_seed_read_via_probe() {
    if !backends::path_enforcement_available() {
        eprintln!("SKIP: isol8-winhook.dll not found");
        return;
    }
    let Some(probe) = probe_path() else {
        eprintln!("SKIP: build isol8-probe first");
        return;
    };

    let root = std::env::temp_dir().join(format!("isol8-it-{}", std::process::id()));
    let seed = root.join("seed");
    let home = root.join("home");
    fs::create_dir_all(&seed).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(seed.join("data.txt"), "seeded\n").unwrap();

    let sysroot = std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".into());
    let profile = Profile {
        paths: vec![
            grant(&sysroot, Access::Ro, MatchKind::Subpath),
            grant(root.to_str().unwrap(), Access::Rw, MatchKind::Subpath),
            grant(seed.to_str().unwrap(), Access::Ro, MatchKind::Subpath),
        ],
        ..Default::default()
    };

    let target = seed.join("data.txt");
    let mut cmd = vec![
        probe.to_string_lossy().into_owned(),
        "read".into(),
        target.to_string_lossy().into_owned(),
    ];
    let mut profile = profile;
    confine_executable(&mut profile, &mut cmd).unwrap();
    let env = build_minimal(&profile, &home, &[], &[]);
    let code = backends::select()
        .spawn(&profile, &env, &cmd)
        .and_then(|mut child| child.wait())
        .expect("spawn");
    assert_eq!(code, 0, "probe read seed/data.txt failed with code {code}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn field_test_scenario_04_replica() {
    if !backends::path_enforcement_available() {
        eprintln!("SKIP: isol8-winhook.dll not found");
        return;
    }
    let Some(probe) = probe_path() else {
        eprintln!("SKIP: build isol8-probe first");
        return;
    };

    let root = std::env::temp_dir().join(format!("isol8-ft-{}", std::process::id()));
    let home = root.join("home");
    let seed = root.join("seed");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&seed).unwrap();
    fs::write(seed.join("data.txt"), "seeded\n").unwrap();

    let sd = seed.to_str().unwrap();
    let sysroot = std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".into());
    let paths = vec![
        grant(&sysroot, Access::Ro, MatchKind::Subpath),
        grant(root.to_str().unwrap(), Access::Rw, MatchKind::Subpath),
        grant(sd, Access::Ro, MatchKind::Subpath),
    ];
    let profile = Profile {
        paths,
        windows: None,
        ..Default::default()
    };

    let target = seed.join("data.txt").to_string_lossy().into_owned();
    let mut cmd = vec![probe.to_string_lossy().into_owned(), "read".into(), target];
    let mut profile = profile;
    confine_executable(&mut profile, &mut cmd).unwrap();
    let env = build_minimal(&profile, &home, &[], &[]);
    let code = backends::select()
        .spawn(&profile, &env, &cmd)
        .and_then(|mut child| child.wait())
        .expect("spawn");
    assert_eq!(code, 0, "field-test replica failed with {code}");
    let _ = fs::remove_dir_all(&root);
}
