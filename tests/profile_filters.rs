//! Integration tests for profile filters, auto-selection, and conditional layers.
//! Exercises the public resolve pipeline without spawning a sandboxed process.

use isol8::filter::{self, RunContext};
use isol8::profile::{self, LayerRegistry};
use isol8::resolve;
use isol8::sandbox::Spec;

fn os_system_profile() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos/system-runtime",
        "linux" => "linux/system-runtime",
        "windows" => "windows/system-runtime",
        _ => "base",
    }
}

/// Build a `Spec` with the given profiles + command; all other fields default.
fn spec(profiles: &[&str], cmd: &[&str]) -> Spec {
    let mut run = Spec::new(cmd.iter().map(|s| s.to_string()));
    run.profiles = profiles.iter().map(|s| (*s).to_string()).collect();
    run
}

fn run_with(cmd: &[&str], auto_profiles: bool, profiles: &[&str]) -> Spec {
    let mut names = vec!["base".to_string(), os_system_profile().to_string()];
    names.extend(profiles.iter().map(|s| (*s).to_string()));
    let mut run = Spec::new(cmd.iter().map(|s| s.to_string()));
    run.profiles = names;
    run.auto_profiles = auto_profiles;
    run
}

fn select_names(run: &Spec) -> Vec<String> {
    let registry = LayerRegistry::load(&run.profile_paths).unwrap();
    let ctx = RunContext::from_cmd(&run.cmd);
    profile::select_layer_names(run, &registry, &ctx).unwrap()
}

/// Grants in the **effective** policy — deliberately via `resolve::effective_policy`
/// rather than a layer-level helper. These assertions used to run against a helper
/// that applied filters correctly while the real pipeline did not, so they passed
/// while OS- and executable-filtered layers leaked into every enforced policy.
/// Any test about what a filter does must observe what the backend actually gets.
fn layer_paths(run: &Spec) -> Vec<String> {
    resolve::effective_policy(run)
        .unwrap()
        .profile
        .paths
        .into_iter()
        .map(|g| g.path)
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

    let mut matching = spec(&["overlay"], &["special"]);
    matching.profile_paths = vec![overlay.path()];
    let mut other = spec(&["overlay"], &["other"]);
    other.profile_paths = vec![overlay.path()];

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
    let run = spec(&["base", mismatch_layer], &["echo", "hi"]);
    let effective = resolve::effective_policy(&run).unwrap();
    let granted: Vec<String> = effective
        .profile
        .paths
        .iter()
        .map(|g| g.path.clone())
        .collect();

    // Every grant unique to the foreign-OS layer must be absent from the merged
    // policy. Comparing against the unfiltered builtin (rather than a hardcoded
    // path) keeps this honest if the builtin layer's contents change.
    let base_paths: Vec<String> = registry
        .get("base")
        .expect("builtin base layer")
        .paths
        .iter()
        .map(|g| g.path.clone())
        .collect();
    for grant in &builtin.paths {
        if base_paths.contains(&grant.path) {
            continue; // also granted by base; not attributable to the filtered layer
        }
        assert!(
            !granted.contains(&grant.path),
            "OS-mismatched layer leaked {:?} into the effective policy; got {granted:?}",
            grant.path
        );
    }

    // The layer shell must survive so `requires` still resolves and ordering holds.
    assert!(
        effective
            .layer_names
            .iter()
            .any(|(n, _)| n == mismatch_layer),
        "filtered layer must stay in the stack for ordering; got {:?}",
        effective.layer_names
    );
}

#[test]
fn os_filtered_layer_contributes_no_grants_to_effective_policy() {
    // Regression: `apply_layer_filter` was only reachable from a helper that no
    // live caller used, so a `filter = { os = [...] }` layer contributed its
    // grants on every platform. A Windows-only rw grant reached the macOS
    // Seatbelt policy and the Linux Landlock ruleset alike.
    let foreign = match std::env::consts::OS {
        "windows" => "linux",
        _ => "windows",
    };
    let overlay = TempOverlay::new(
        "os-filter-leak",
        &format!(
            r#"
filter = {{ os = ["{foreign}"] }}
paths = [{{ path = "/isol8-test-foreign-os-grant", access = "rw" }}]
"#
        ),
    );
    let mut run = spec(&["base", "overlay"], &["echo", "hi"]);
    run.profile_paths = vec![overlay.path()];

    let paths = layer_paths(&run);
    assert!(
        !has_grant(&paths, "/isol8-test-foreign-os-grant"),
        "layer filtered to os={foreign:?} must contribute nothing on {}; got {paths:?}",
        std::env::consts::OS
    );
}

#[test]
fn rewrite_from_non_matching_layer_does_not_reach_command() {
    // Regression: the same missing filter step let a layer's `rewrite` apply to
    // any command. Naming three agent layers injected all three auto-approve
    // flags into an unrelated binary — silently disabling other tools' own
    // confirmation prompts.
    let overlay = TempOverlay::new(
        "rewrite-leak",
        r#"
filter = { executables = ["definitely-not-echo"] }
rewrite = { ensure_args = ["--isol8-test-should-not-appear"] }
"#,
    );
    let mut run = spec(&["base", "overlay"], &["echo", "hi"]);
    run.profile_paths = vec![overlay.path()];

    let cmd = resolve::effective_policy(&run).unwrap().cmd;
    assert!(
        !cmd.iter().any(|a| a == "--isol8-test-should-not-appear"),
        "rewrite from an executable-filtered layer must not reach an unrelated command; got {cmd:?}"
    );

    // ...and still applies when the filter *does* match.
    let mut matching = spec(&["base", "overlay"], &["definitely-not-echo"]);
    matching.profile_paths = vec![overlay.path()];
    let cmd = resolve::effective_policy(&matching).unwrap().cmd;
    assert!(
        cmd.iter().any(|a| a == "--isol8-test-should-not-appear"),
        "rewrite must still apply to the matching executable; got {cmd:?}"
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
    let mut run = spec(&[alias], &["claude"]);
    run.auto_profiles = true;
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
    let real = std::path::PathBuf::from(match std::env::consts::OS {
        "windows" => std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .expect("USERPROFILE or HOME set in test env"),
        _ => std::env::var_os("HOME").expect("HOME set in test env"),
    });
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
    let mut run = spec(&["base", os_system_profile(), "overlay"], &["echo", "hi"]);
    run.profile_paths = vec![overlay.path()];
    let effective = resolve::effective_policy(&run).unwrap();
    assert_eq!(
        effective.home.path, replacement,
        "profile home_replace must override the real home"
    );
}

#[test]
fn confine_executable_absolutizes_and_grants_binary() {
    let exe = match std::env::consts::OS {
        "windows" => {
            let root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".into());
            format!("{root}\\System32\\cmd.exe")
        }
        _ => "/bin/sh".into(),
    };
    let run = run_with(&[&exe], false, &[]);
    let mut effective = resolve::effective_policy(&run).unwrap();
    resolve::confine_executable(&mut effective.profile, &mut effective.cmd).unwrap();
    assert_eq!(effective.cmd[0], exe);
    assert!(
        effective.profile.paths.iter().any(|g| g.path == exe),
        "resolved binary must be auto-granted; got {:?}",
        effective.profile.paths
    );
}

/// Every agent layer pulls `integrations/macos-gui`: TUIs need HIToolbox, input
/// methods and the window server even when nothing renders a window.
#[test]
fn every_agent_layer_requires_macos_gui() {
    let registry = LayerRegistry::load(&[]).unwrap();
    let mut agents: Vec<String> = registry
        .list()
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| n.starts_with("agents/"))
        .collect();
    agents.sort();
    assert!(!agents.is_empty(), "expected builtin agents/* layers");
    for name in &agents {
        let layer = registry.get(name).unwrap();
        assert!(
            layer.requires.iter().any(|r| r == "integrations/macos-gui"),
            "{name} does not require integrations/macos-gui"
        );
    }
    // And it actually lands in a resolved stack.
    let run = spec(&["base", &agents[0]], &["echo", "hi"]);
    let effective = resolve::effective_policy(&run).unwrap();
    assert!(
        effective
            .layer_names
            .iter()
            .any(|(n, _)| n == "integrations/macos-gui"),
        "layers: {:?}",
        effective.layer_names
    );
}
