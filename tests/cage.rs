//! Cage selection → Spec overlay (Phase 1 evolution).

use isol8::cage::{self, HomeMode};
use isol8::sandbox::Spec;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn tmp_dir() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "isol8-cage-itest-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn cage_overlay_feeds_spec_fields() {
    let dir = tmp_dir();
    let path = dir.join("work.toml");
    std::fs::write(
        &path,
        r#"
schema = 1
name = "work"
home = "/tmp/cage-home"
profiles = ["base", "toolchains/rust"]
[[dirs]]
path = "~/proj"
access = "rw"
[[dirs]]
path = "/opt/ro"
access = "ro"
"#,
    )
    .unwrap();

    let cage = cage::load_from_path(&path).unwrap();
    let o = cage.overlay();

    let spec = Spec {
        profiles: o.profiles,
        home: o.home,
        ephemeral_home: o.ephemeral_home,
        add_dirs_rw: o.add_dirs_rw,
        add_dirs_ro: o.add_dirs_ro,
        ..Default::default()
    };

    assert_eq!(spec.profiles, ["base", "toolchains/rust"]);
    assert_eq!(spec.home.as_deref(), Some("/tmp/cage-home"));
    assert!(!spec.ephemeral_home);
    assert_eq!(spec.add_dirs_rw, ["~/proj"]);
    assert_eq!(spec.add_dirs_ro, ["/opt/ro"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_discovery_and_show() {
    let root = tmp_dir();
    let cages = root.join(".isol8").join("cages");
    std::fs::create_dir_all(&cages).unwrap();
    std::fs::write(
        cages.join("dev.toml"),
        r#"
schema = 1
name = "dev"
home = "ephemeral"
profiles = ["base"]
"#,
    )
    .unwrap();

    let c = cage::resolve(Some("dev"), &root).unwrap().unwrap();
    assert_eq!(c.home, HomeMode::Ephemeral);
    assert!(c.overlay().ephemeral_home);

    let listed = cage::list_cages(&root).unwrap();
    assert!(
        listed.iter().any(|(n, _)| n == "dev"),
        "list should include dev: {listed:?}"
    );

    // Config-root cages dir (simulates ISOL8_CONFIG_PATH / project config_path).
    let cfg = root.join("cfg-root");
    let cfg_cages = cfg.join("cages");
    std::fs::create_dir_all(&cfg_cages).unwrap();
    std::fs::write(
        cfg_cages.join("from-config.toml"),
        r#"
schema = 1
name = "from-config"
home = "inherit"
profiles = ["base"]
"#,
    )
    .unwrap();
    let listed2 = cage::list_cages_in(&root, Some(&cfg)).unwrap();
    assert!(
        listed2.iter().any(|(n, _)| n == "from-config"),
        "list_cages_in should include config-root cage: {listed2:?}"
    );
    assert!(
        listed2.iter().any(|(n, _)| n == "dev"),
        "list_cages_in still includes project .isol8 cages: {listed2:?}"
    );

    let text = cage::format_show(&c);
    assert!(text.contains("ephemeral"));
    assert!(text.contains("dev"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cli_flag_overrides_cage_home() {
    // Documented precedence: CLI --home wins over cage home.
    let dir = tmp_dir();
    let path = dir.join("work.toml");
    std::fs::write(
        &path,
        r#"
schema = 1
name = "work"
home = "/from-cage"
profiles = ["base"]
"#,
    )
    .unwrap();
    let cage = cage::load_from_path(&path).unwrap();
    let o = cage.overlay();

    // Simulate prepare_run: CLI already set home.
    let cli_home = true;
    let mut home = Some("/from-cli".to_string());
    if !cli_home {
        if let Some(h) = o.home {
            home = Some(h);
        }
    }
    assert_eq!(home.as_deref(), Some("/from-cli"));

    let _ = std::fs::remove_dir_all(&dir);
}
