//! Phase 4: detect + verify integration.

use isol8::detect::{self, commands_trusted};
use isol8::filter::RunContext;
use isol8::recipe::{RecipeRegistry, StrategyName, ToolchainChoice};
use isol8::sandbox::Spec;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn tmp() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "isol8-dv-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn detect_all_lists_platform_recipes() {
    let reg = RecipeRegistry::load(&[]).unwrap();
    let ctx = RunContext::from_cmd(&[]);
    let real = isol8::context::real_home_from_env();
    let rows = detect::detect_all(&reg, &ctx, &real).unwrap();
    assert!(!rows.is_empty(), "expected at least one platform recipe");
    let table = detect::format_detect_table(&rows);
    assert!(table.contains("Detected"));
}

#[test]
fn verify_fixture_echo_recipe() {
    if matches!(std::env::consts::OS, "windows") {
        // AppContainer capture is best-effort; skip heavy path on Windows CI.
        return;
    }

    let dir = tmp();
    let recipes = dir.join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    // Recipe that always verifies with /bin/echo (no host install needed).
    std::fs::write(
        recipes.join("echo.toml"),
        r##"
schema = 1
id = "toolchains/echo"
kind = "recipe"
filter = { os = ["macos", "linux"] }
summary = "echo smoke"
[detect]
probe = { path = "~" }
[verify]
cmd = "/bin/echo ok-verify"
expect = "^ok-verify"
[strategies.isolate]
home = [{ kind = "mkdir", path = "~/.echo" }]
paths = [{ path = "~/.echo", access = "rw" }]
"##,
    )
    .unwrap();

    let home = dir.join("home");
    let system = match std::env::consts::OS {
        "macos" => "macos/system-runtime",
        "linux" => "linux/system-runtime",
        _ => "base",
    };
    let mut spec = Spec::new(["true"]);
    spec.profiles = vec!["base".into(), system.into()];
    spec.home = Some(home.to_string_lossy().into_owned());
    spec.recipe_paths = vec![recipes.to_string_lossy().into_owned()];
    spec.toolchains = vec![ToolchainChoice {
        id: "toolchains/echo".into(),
        strategy: StrategyName::Isolate,
    }];

    let results = detect::verify_toolchains(&spec).unwrap();
    let report = detect::format_verify_report(&results);
    assert!(
        results.iter().any(|r| r.id == "toolchains/echo" && r.ok),
        "report:\n{report}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn trust_gate_helpers() {
    assert!(commands_trusted("builtin:toolchains/nvm"));
    assert!(!commands_trusted("https://evil/r.toml"));
}
