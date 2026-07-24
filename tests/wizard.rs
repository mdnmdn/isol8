//! Cage wizard non-interactive authoring (Phase 8).

use isol8::profile::Access;
use isol8::recipe::{StrategyName, ToolchainChoice};
use isol8::wizard::{
    apply, check_drift, load_state, managed_hash_from_body, parse_tools_list, render, DriftStatus,
    WizardRequest,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn tmp_dir() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "isol8-wizard-itest-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn noninteractive_new_golden_shape() {
    let dir = tmp_dir();
    let state = dir.join("state.toml");
    let req = WizardRequest {
        name: "work".into(),
        home: "managed".into(),
        tools: parse_tools_list("nvm,cargo:share", None).unwrap(),
        dirs: vec![("~/proj".into(), Access::Rw)],
        profiles: vec!["base".into()],
        out_dir: Some(dir.clone()),
        force: false,
        existing_path: None,
    };
    let r = apply(&req, &state).unwrap();
    let body = std::fs::read_to_string(&r.path).unwrap();

    assert!(body.contains("name = \"work\""));
    assert!(body.contains("home = \"@managed/work\""));
    assert!(body.contains("profiles = ["));
    assert!(body.contains("\"base\""));
    assert!(body.contains("[toolchains.nvm]"));
    assert!(body.contains("strategy = \"link\""));
    assert!(body.contains("[toolchains.cargo]"));
    assert!(body.contains("strategy = \"share\""));
    assert!(body.contains("path = \"~/proj\""));
    assert!(body.contains("isol8:managed"));

    // Round-trip load via cage parser
    let cage = isol8::cage::load_from_path(&r.path).unwrap();
    assert_eq!(cage.name, "work");
    assert_eq!(cage.toolchains.len(), 2);
    assert_eq!(cage.dirs.len(), 1);

    let st = load_state(&state).unwrap();
    assert!(st.cages.contains_key("work"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edit_preserves_dirs_rewrites_toolchains() {
    let dir = tmp_dir();
    let state = dir.join("state.toml");
    let req = WizardRequest {
        name: "e".into(),
        home: "inherit".into(),
        tools: vec![ToolchainChoice {
            id: "toolchains/nvm".into(),
            strategy: StrategyName::Link,
        }],
        dirs: vec![("~/keep".into(), Access::Rw)],
        profiles: vec![],
        out_dir: Some(dir.clone()),
        force: false,
        existing_path: None,
    };
    let r = apply(&req, &state).unwrap();

    let edit = WizardRequest {
        name: "e".into(),
        home: "managed".into(),
        tools: vec![ToolchainChoice {
            id: "toolchains/cargo".into(),
            strategy: StrategyName::Link,
        }],
        dirs: vec![], // must not wipe existing dirs
        profiles: vec![],
        out_dir: Some(dir.clone()),
        force: false,
        existing_path: Some(r.path.clone()),
    };
    let r2 = apply(&edit, &state).unwrap();
    let body = std::fs::read_to_string(&r2.path).unwrap();
    assert!(body.contains("keep"), "user dir preserved:\n{body}");
    assert!(body.contains("cargo"));
    assert!(!body.contains("[toolchains.nvm]"));
    assert!(body.contains("@managed/e"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preview_render_no_write() {
    let dir = tmp_dir();
    let req = WizardRequest {
        name: "p".into(),
        home: "ephemeral".into(),
        tools: vec![],
        dirs: vec![],
        profiles: vec![],
        out_dir: Some(dir.clone()),
        force: false,
        existing_path: None,
    };
    let r = render(&req).unwrap();
    assert!(r.body.contains("ephemeral"));
    assert!(!r.path.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hand_edit_sets_drift() {
    let dir = tmp_dir();
    let state = dir.join("state.toml");
    let req = WizardRequest {
        name: "d".into(),
        home: "inherit".into(),
        tools: vec![ToolchainChoice {
            id: "toolchains/nvm".into(),
            strategy: StrategyName::Link,
        }],
        dirs: vec![],
        profiles: vec![],
        out_dir: Some(dir.clone()),
        force: false,
        existing_path: None,
    };
    let r = apply(&req, &state).unwrap();
    let st = load_state(&state).unwrap();
    assert_eq!(
        check_drift("d", &r.path, &st).unwrap(),
        DriftStatus::Unchanged
    );

    let mut body = std::fs::read_to_string(&r.path).unwrap();
    body = body.replace("link", "share");
    std::fs::write(&r.path, &body).unwrap();
    let actual = managed_hash_from_body(&body).unwrap();
    assert_ne!(actual, r.managed_hash);
    assert!(matches!(
        check_drift("d", &r.path, &st).unwrap(),
        DriftStatus::Drift { .. }
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
