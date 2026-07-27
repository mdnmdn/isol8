//! Recipe + cage toolchain integration (Phase 3).

use isol8::cage;
use isol8::recipe::{RecipeRegistry, StrategyName, ToolchainChoice};
use isol8::sandbox::{self, Spec};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn tmp_dir() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "isol8-recipe-itest-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn builtin_registry_has_nvm() {
    let reg = RecipeRegistry::load(&[]).unwrap();
    assert!(
        reg.ids().iter().any(|id| id == "toolchains/nvm"),
        "expected embedded toolchains/nvm, got {:?}",
        reg.ids()
    );
}

#[test]
fn dry_run_applies_recipe_from_spec() {
    // Use a platform that matches nvm recipe (macos/linux). Skip on windows.
    if matches!(std::env::consts::OS, "windows") {
        return;
    }
    let spec = Spec {
        profiles: vec!["base".into()],
        home: Some("/tmp/isol8-recipe-home".into()),
        toolchains: vec![ToolchainChoice {
            id: "toolchains/nvm".into(),
            strategy: StrategyName::Link,
        }],
        cmd: vec!["echo".into(), "hi".into()],
        ..Default::default()
    };
    let dry = sandbox::dry_run(&spec).unwrap();
    assert!(
        dry.recipes
            .iter()
            .any(|(id, s)| id == "toolchains/nvm" && s == "link"),
        "recipes: {:?}",
        dry.recipes
    );
    // Link home op planned
    let plan = dry.home_plan.render();
    assert!(
        plan.contains("link") || plan.contains(".nvm"),
        "plan:\n{plan}"
    );
    // Env expanded
    assert_eq!(
        dry.env.get("NVM_DIR").map(String::as_str),
        Some("/tmp/isol8-recipe-home/.nvm")
    );
    // Path grants include real-home nvm target
    assert!(
        dry.profile.paths.iter().any(|g| g.path.contains(".nvm")),
        "paths: {:?}",
        dry.profile
            .paths
            .iter()
            .map(|g| &g.path)
            .collect::<Vec<_>>()
    );
}

#[test]
fn cage_toolchains_feed_spec() {
    let dir = tmp_dir();
    let path = dir.join("work.toml");
    std::fs::write(
        &path,
        r#"
schema = 1
name = "work"
home = "/tmp/cage-recipe-home"
profiles = ["base"]
[toolchains.nvm]
strategy = "link"
"#,
    )
    .unwrap();
    let cage = cage::load_from_path(&path).unwrap();
    let o = cage.overlay();
    assert_eq!(o.toolchains.len(), 1);
    assert_eq!(o.toolchains[0].id, "toolchains/nvm");
    assert_eq!(o.toolchains[0].strategy, StrategyName::Link);

    if matches!(std::env::consts::OS, "windows") {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let spec = Spec {
        profiles: o.profiles,
        home: o.home,
        toolchains: o.toolchains,
        cmd: vec!["echo".into()],
        ..Default::default()
    };
    let dry = sandbox::dry_run(&spec).unwrap();
    assert!(!dry.recipes.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// --- strategy-level platform selectors ---------------------------------------
//
// A strategy name may carry several bodies with disjoint filters, so one recipe
// expresses per-platform internals without splitting into variant files. This is
// the same authoritative-selector rule as recipes and profile layers; the tests
// below pin the three behaviours that make it safe: the matching body wins, an
// unmatched strategy is a clear error rather than a silent empty contribution,
// and overlapping selectors are rejected at parse time.

fn write_recipe(dir: &std::path::Path, name: &str, body: &str) -> String {
    let p = dir.join(format!("{name}.toml"));
    std::fs::write(&p, body).unwrap();
    dir.to_string_lossy().into_owned()
}

const HOST_OS: &str = std::env::consts::OS;

fn foreign_os() -> &'static str {
    if HOST_OS == "linux" {
        "macos"
    } else {
        "linux"
    }
}

#[test]
fn strategy_body_selected_by_platform_filter() {
    let dir = tmp_dir();
    let path = write_recipe(
        &dir,
        "split",
        &format!(
            r#"
schema = 1
id = "toolchains/split"
kind = "recipe"
summary = "per-platform strategy bodies"

[[strategies.link]]
filter = {{ os = ["{host}"] }}
paths = [{{ path = "/isol8-host-body", access = "ro" }}]
env = {{ SPLIT_BODY = "host" }}

[[strategies.link]]
filter = {{ os = ["{foreign}"] }}
paths = [{{ path = "/isol8-foreign-body", access = "ro" }}]
env = {{ SPLIT_BODY = "foreign" }}
"#,
            host = HOST_OS,
            foreign = foreign_os()
        ),
    );

    let reg = RecipeRegistry::load(&[path]).unwrap();
    let ctx = isol8::filter::RunContext::from_cmd(&["echo".to_string()]);
    let c = reg
        .compile(
            &ToolchainChoice {
                id: "toolchains/split".into(),
                strategy: StrategyName::Link,
            },
            &ctx,
        )
        .unwrap();

    let paths: Vec<&str> = c.paths.iter().map(|g| g.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["/isol8-host-body"],
        "only the body whose selector matches {HOST_OS} may contribute"
    );
    assert_eq!(c.env.get("SPLIT_BODY").map(String::as_str), Some("host"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn strategy_with_no_matching_body_is_an_error() {
    let dir = tmp_dir();
    let path = write_recipe(
        &dir,
        "foreign-only",
        &format!(
            r#"
schema = 1
id = "toolchains/foreign-only"
kind = "recipe"
summary = "strategy that does not exist on this platform"

[[strategies.link]]
filter = {{ os = ["{foreign}"] }}
paths = [{{ path = "/isol8-foreign-body", access = "ro" }}]
"#,
            foreign = foreign_os()
        ),
    );

    let reg = RecipeRegistry::load(&[path]).unwrap();
    let ctx = isol8::filter::RunContext::from_cmd(&["echo".to_string()]);
    let err = reg
        .compile(
            &ToolchainChoice {
                id: "toolchains/foreign-only".into(),
                strategy: StrategyName::Link,
            },
            &ctx,
        )
        .expect_err("a strategy with no body for this platform must not resolve silently");
    let msg = err.to_string();
    assert!(
        msg.contains("no body matching this platform"),
        "error should name the cause; got: {msg}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overlapping_strategy_bodies_rejected_at_parse() {
    let dir = tmp_dir();
    let path = write_recipe(
        &dir,
        "ambiguous",
        r#"
schema = 1
id = "toolchains/ambiguous"
kind = "recipe"
summary = "two bodies that both match everything"

[[strategies.link]]
paths = [{ path = "/a", access = "ro" }]

[[strategies.link]]
paths = [{ path = "/b", access = "ro" }]
"#,
    );

    let err = RecipeRegistry::load(&[path])
        .expect_err("overlapping strategy selectors must be rejected, not silently ordered");
    let msg = err.to_string();
    assert!(
        msg.contains("overlapping filters"),
        "error should explain the overlap; got: {msg}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn single_table_strategy_form_still_parses() {
    // `[strategies.link]` (a table, no filter) must keep working unchanged —
    // the variant form is opt-in and every existing recipe uses this shape.
    let dir = tmp_dir();
    let path = write_recipe(
        &dir,
        "plain",
        r#"
schema = 1
id = "toolchains/plain"
kind = "recipe"
summary = "single-body strategy"

[strategies.link]
paths = [{ path = "/isol8-plain", access = "ro" }]
"#,
    );

    let reg = RecipeRegistry::load(&[path]).unwrap();
    let ctx = isol8::filter::RunContext::from_cmd(&["echo".to_string()]);
    let c = reg
        .compile(
            &ToolchainChoice {
                id: "toolchains/plain".into(),
                strategy: StrategyName::Link,
            },
            &ctx,
        )
        .unwrap();
    assert_eq!(c.paths.len(), 1);
    assert_eq!(c.paths[0].path, "/isol8-plain");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_strategy_field_names_the_offending_key() {
    // Strategy bodies are hand-dispatched on value shape rather than parsed via
    // an untagged enum, precisely so a typo reports the field instead of
    // "data did not match any variant". Pin that: a bad error here sends people
    // hunting through a whole recipe for a one-character mistake.
    let dir = tmp_dir();
    let path = write_recipe(
        &dir,
        "typo",
        r#"
schema = 1
id = "toolchains/typo"
kind = "recipe"
summary = "field typo"

[strategies.link]
pathz = [{ path = "/a", access = "ro" }]
"#,
    );

    let msg = RecipeRegistry::load(&[path])
        .expect_err("unknown field must be rejected")
        .to_string();
    assert!(
        msg.contains("pathz") && msg.contains("[strategies.link]"),
        "error must name the bad key and its location; got: {msg}"
    );
    assert!(
        !msg.contains("untagged"),
        "error must not leak serde internals; got: {msg}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Registry-schema recipe: `requires` joins the layer stack and `path_prepend`
/// lands at the front of PATH (globs expanded against the real home).
#[test]
fn recipe_requires_and_path_prepend_apply() {
    if matches!(std::env::consts::OS, "windows") {
        return;
    }
    let dir = tmp_dir();
    let real_home = std::env::var("HOME").unwrap();
    let shims = PathBuf::from(&real_home).join(".isol8-test-shims/v1/bin");
    std::fs::create_dir_all(&shims).unwrap();

    std::fs::write(
        dir.join("tool.toml"),
        r##"
schema = 1
id = "toolchains/pp-test"
kind = "recipe"
summary = "path_prepend fixture"
tags = ["test"]
requires = ["integrations/git"]

[strategies.link]
summary = "link"
paths = [{ path = "#HOME/.isol8-test-shims", access = "ro" }]
path_prepend = ["#HOME/.isol8-test-shims/*/bin", "~/.local/bin"]
"##,
    )
    .unwrap();

    let home = dir.join("home");
    let spec = Spec {
        profiles: vec!["base".into()],
        home: Some(home.to_string_lossy().into_owned()),
        recipe_paths: vec![dir.to_string_lossy().into_owned()],
        toolchains: vec![ToolchainChoice {
            id: "toolchains/pp-test".into(),
            strategy: StrategyName::Link,
        }],
        cmd: vec!["echo".into(), "hi".into()],
        ..Default::default()
    };
    let dry = sandbox::dry_run(&spec).unwrap();

    // `requires` pulled the layer in (tagged as required, not explicit).
    assert!(
        dry.layer_names.iter().any(|(n, _)| n == "integrations/git"),
        "layers: {:?}",
        dry.layer_names.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );

    let path = dry.env.get("PATH").cloned().unwrap_or_default();
    let entries: Vec<&str> = path.split(':').collect();
    assert_eq!(
        entries.first().copied(),
        Some(shims.to_string_lossy().as_ref()),
        "PATH: {path}"
    );
    assert_eq!(
        entries.get(1).copied(),
        Some(home.join(".local/bin").to_string_lossy().as_ref()),
        "PATH: {path}"
    );

    std::fs::remove_dir_all(PathBuf::from(&real_home).join(".isol8-test-shims")).ok();
    std::fs::remove_dir_all(&dir).ok();
}
