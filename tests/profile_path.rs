//! Profile-path overlay and auto-profile selection.

use isol8::cli::{self, ProfileOpts};
use isol8::filter::RunContext;
use isol8::profile::LayerRegistry;

#[test]
fn profile_path_single_file_overrides_builtin() {
    let dir = std::env::temp_dir().join(format!("isol8-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("override.toml");
    std::fs::write(
        &path,
        r#"
paths = [{ path = "/custom-only", access = "rw" }]
"#,
    )
    .unwrap();

    let registry = LayerRegistry::load(&[path.to_string_lossy().into_owned()]).unwrap();
    // base is still builtin; our override adds a layer named "override".
    assert!(registry.get("override").is_some());
    assert!(registry.get("base").is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn auto_select_claude_agent_layer() {
    let registry = LayerRegistry::load(&[]).unwrap();
    let run = cli::run_from(
        ProfileOpts {
            profiles: vec!["base".into(), "macos/system-runtime".into()],
            auto_profiles: true,
            ..Default::default()
        },
        vec!["claude".into(), "--version".into()],
    );
    let ctx = RunContext::from_cmd(&run.cmd);
    let names = isol8::profile::select_layer_names(&run, &registry, &ctx).unwrap();
    assert!(names.contains(&"agents/claude-code".to_string()));
}
