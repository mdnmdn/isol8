//! Integration tests for profile filters, auto-selection, and conditional layers.
//! Exercises the public resolve pipeline without spawning a sandboxed process.

use isol8::cli::{self, ProfileOpts};
use isol8::filter::{self, RunContext};
use isol8::profile::{self, LayerRegistry};
use isol8::resolve;

fn os_system_profile() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos/system-runtime",
        "linux" => "linux/system-runtime",
        "windows" => "windows/system-runtime",
        _ => "base",
    }
}

fn run_with(cmd: &[&str], auto_profiles: bool, profiles: &[&str]) -> cli::RunArgs {
    let mut names = vec!["base".into(), os_system_profile().into()];
    names.extend(profiles.iter().map(|s| (*s).to_string()));
    cli::run_from(
        ProfileOpts {
            profiles: names,
            auto_profiles,
            ..Default::default()
        },
        cmd.iter().map(|s| s.to_string()).collect(),
    )
}

fn select_names(run: &cli::RunArgs) -> Vec<String> {
    let registry = LayerRegistry::load(run.profile_paths()).unwrap();
    let ctx = RunContext::from_cmd(&run.cmd);
    profile::select_layer_names(run, &registry, &ctx).unwrap()
}

fn layer_paths(run: &cli::RunArgs) -> Vec<String> {
    profile::resolved_layers(run)
        .unwrap()
        .into_iter()
        .flat_map(|l| l.paths.into_iter().map(|g| g.path))
        .collect()
}

fn has_grant(paths: &[String], needle: &str) -> bool {
    paths.iter().any(|p| p.contains(needle))
}

struct TempOverlay {
    dir: std::path::PathBuf,
}

impl TempOverlay {
    fn new(name: &str, body: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("isol8-filter-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("overlay.toml"), body).unwrap();
        Self { dir }
    }

    fn path(&self) -> String {
        self.dir.to_string_lossy().into_owned()
    }
}

impl Drop for TempOverlay {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn auto_select_claude_by_executable_basename() {
    let run = run_with(&["claude", "--version"], true, &[]);
    let names = select_names(&run);
    assert!(
        names.contains(&"agents/claude-code".to_string()),
        "expected agents/claude-code in {names:?}"
    );
}

#[test]
fn auto_select_claude_with_full_executable_path() {
    let run = run_with(&["/usr/bin/claude", "--version"], true, &[]);
    let names = select_names(&run);
    assert!(
        names.contains(&"agents/claude-code".to_string()),
        "basename match should auto-select; got {names:?}"
    );
}

#[test]
fn auto_select_skips_agent_for_unrelated_executable() {
    let run = run_with(&["cargo", "build"], true, &[]);
    let names = select_names(&run);
    assert!(
        !names.contains(&"agents/claude-code".to_string()),
        "cargo must not pull agents/claude-code; got {names:?}"
    );
}

#[test]
fn auto_profiles_disabled_skips_executable_layers() {
    let run = run_with(&["claude", "--version"], false, &[]);
    let names = select_names(&run);
    assert!(
        !names.contains(&"agents/claude-code".to_string()),
        "auto_profiles=false must not auto-select; got {names:?}"
    );
}

#[test]
fn explicit_profile_selected_regardless_of_executable() {
    let run = run_with(&["cargo", "build"], false, &["agents/claude-code"]);
    let names = select_names(&run);
    assert!(
        names.contains(&"agents/claude-code".to_string()),
        "explicit --profile must select layer; got {names:?}"
    );
}

#[test]
fn resolved_layers_include_claude_grants_only_for_claude_cmd() {
    let claude = run_with(&["claude"], true, &[]);
    let cargo = run_with(&["cargo", "build"], true, &[]);

    let claude_paths = layer_paths(&claude);
    let cargo_paths = layer_paths(&cargo);

    assert!(
        has_grant(&claude_paths, ".claude"),
        "claude cmd should fold agents/claude-code grants; got {claude_paths:?}"
    );
    assert!(
        !has_grant(&cargo_paths, ".claude"),
        "cargo cmd must not include claude agent grants; got {cargo_paths:?}"
    );
}

#[test]
fn policy_executable_filter_folds_only_for_matching_cmd() {
    let overlay = TempOverlay::new(
        "policy-exe",
        r#"
paths = [{ path = "/always", access = "rw" }]
[[policies]]
filter = { executables = ["special"] }
paths = [{ path = "/only-special", access = "rw" }]
"#,
    );

    let matching = cli::run_from(
        ProfileOpts {
            profiles: vec!["overlay".into()],
            profile_paths: vec![overlay.path()],
            ..Default::default()
        },
        vec!["special".into()],
    );
    let other = cli::run_from(
        ProfileOpts {
            profiles: vec!["overlay".into()],
            profile_paths: vec![overlay.path()],
            ..Default::default()
        },
        vec!["other".into()],
    );

    let match_paths = layer_paths(&matching);
    let other_paths = layer_paths(&other);

    assert!(has_grant(&match_paths, "/always"));
    assert!(has_grant(&match_paths, "/only-special"));
    assert!(has_grant(&other_paths, "/always"));
    assert!(
        !has_grant(&other_paths, "/only-special"),
        "policy grant must not fold for non-matching executable; got {other_paths:?}"
    );
}

#[test]
fn os_layer_filter_clears_paths_on_mismatch_but_keeps_requires() {
    let mismatch_layer = match std::env::consts::OS {
        "macos" => "linux/system-runtime",
        "linux" => "macos/system-runtime",
        other => {
            eprintln!("SKIP os_layer_filter_clears_paths_on_mismatch: unsupported OS {other}");
            return;
        }
    };

    let registry = LayerRegistry::load(&[]).unwrap();
    let builtin = registry
        .get(mismatch_layer)
        .unwrap_or_else(|| panic!("builtin layer {mismatch_layer} missing"));
    assert!(
        !builtin.paths.is_empty(),
        "precondition: {mismatch_layer} should carry paths before filtering"
    );

    // Select only base + the foreign OS runtime (no matching system-runtime layer).
    let run = cli::run_from(
        ProfileOpts {
            profiles: vec!["base".into(), mismatch_layer.into()],
            auto_profiles: false,
            ..Default::default()
        },
        vec!["echo".into(), "hi".into()],
    );
    let layers = profile::resolved_layers(&run).unwrap();
    let filtered = layers
        .last()
        .expect("base + explicit layer should yield a two-layer stack");

    assert!(
        filtered.paths.is_empty(),
        "OS-mismatched layer must clear paths; got {:?}",
        filtered.paths
    );
    assert_eq!(
        filtered.requires, builtin.requires,
        "requires must survive layer filter so deps still resolve"
    );
}

#[test]
fn effective_policy_auto_selects_claude_agent_layer() {
    let run = run_with(&["claude"], true, &[]);
    let effective = resolve::effective_policy(&run).unwrap();
    assert!(
        effective
            .layer_names
            .iter()
            .any(|(n, o)| n == "agents/claude-code" && *o == resolve::LayerOrigin::Auto),
        "effective_policy layer stack: {:?}",
        effective.layer_names
    );
    assert!(
        effective
            .profile
            .paths
            .iter()
            .any(|g| g.path.contains(".claude")),
        "merged profile should include claude agent paths"
    );
}

#[test]
fn layer_stack_tags_provenance_explicit_auto_required() {
    // Name only the OS alias (e.g. `macos-system`); `base` is dragged in via
    // `requires`, and `agents/claude-code` is auto-matched by the `claude` command.
    let alias = match std::env::consts::OS {
        "macos" => "macos-system",
        "linux" => "linux-system",
        _ => return, // only the two real backends ship these aliases
    };
    let run = cli::run_from(
        ProfileOpts {
            profiles: vec![alias.into()],
            auto_profiles: true,
            ..Default::default()
        },
        vec!["claude".into()],
    );
    let stack = resolve::effective_policy(&run).unwrap().layer_names;

    let origin = |name: &str| stack.iter().find(|(n, _)| n == name).map(|(_, o)| *o);
    assert_eq!(
        origin(alias),
        Some(resolve::LayerOrigin::Explicit),
        "named layer is explicit; stack: {stack:?}"
    );
    assert_eq!(
        origin("base"),
        Some(resolve::LayerOrigin::Required),
        "base is pulled in transitively; stack: {stack:?}"
    );
    assert_eq!(
        origin("agents/claude-code"),
        Some(resolve::LayerOrigin::Auto),
        "agent layer is auto-matched; stack: {stack:?}"
    );
    // Deps-first: a required dependency precedes the layer that names it.
    let pos = |name: &str| stack.iter().position(|(n, _)| n == name).unwrap();
    assert!(
        pos("base") < pos(alias),
        "deps-first order; stack: {stack:?}"
    );
}

#[test]
fn filter_matches_full_command_path_literal() {
    let f = profile::ProfileFilter {
        executables: vec!["/opt/bin/claude".into()],
        ..Default::default()
    };
    let ctx = RunContext {
        cmd: vec!["/opt/bin/claude".into()],
        os: "macos".into(),
        arch: "aarch64".into(),
    };
    assert!(filter::filter_matches(&f, &ctx));
    assert!(!filter::filter_matches(
        &f,
        &RunContext {
            cmd: vec!["claude".into()],
            ..ctx.clone()
        }
    ));
}

#[test]
fn is_auto_selectable_requires_executable_constraint() {
    assert!(!filter::is_auto_selectable(&None));
    assert!(!filter::is_auto_selectable(&Some(profile::ProfileFilter {
        os: vec!["linux".into()],
        ..Default::default()
    })));
    assert!(filter::is_auto_selectable(&Some(profile::ProfileFilter {
        executables: vec!["claude".into()],
        ..Default::default()
    })));
}

#[test]
fn default_run_keeps_real_home() {
    // With the default stack (base + system-runtime) and no replacement requested,
    // the effective HOME is the real one — HOME replacement is opt-in.
    let run = run_with(&["echo", "hi"], false, &[]);
    let effective = resolve::effective_policy(&run).unwrap();
    let real = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME set in test env"));
    assert_eq!(
        effective.home.path, real,
        "default run must not replace HOME; got {:?}",
        effective.home.path
    );
}

#[test]
fn profile_home_replace_overrides_home() {
    // A profile (loaded from TOML) that opts into HOME replacement drives the
    // effective home through the normal resolve pipeline.
    let replacement = std::env::temp_dir().join("isol8-it-home");
    let overlay = TempOverlay::new(
        "home-replace",
        &format!(
            "home_replace = {{ enabled = true, auto_scratch = false, path = {:?} }}\n",
            replacement.to_string_lossy()
        ),
    );
    let run = cli::run_from(
        ProfileOpts {
            profiles: vec!["base".into(), os_system_profile().into(), "overlay".into()],
            profile_paths: vec![overlay.path()],
            ..Default::default()
        },
        vec!["echo".into(), "hi".into()],
    );
    let effective = resolve::effective_policy(&run).unwrap();
    assert_eq!(
        effective.home.path, replacement,
        "profile home_replace must override the real home"
    );
}

#[test]
fn confine_executable_absolutizes_and_grants_binary() {
    // /bin/sh exists on macOS and Linux; the path branch avoids PATH dependence.
    let run = run_with(&["/bin/sh"], false, &[]);
    let mut effective = resolve::effective_policy(&run).unwrap();
    resolve::confine_executable(&mut effective.profile, &mut effective.cmd).unwrap();
    assert_eq!(effective.cmd[0], "/bin/sh");
    assert!(
        effective.profile.paths.iter().any(|g| g.path == "/bin/sh"),
        "resolved binary must be auto-granted; got {:?}",
        effective.profile.paths
    );
}
