//! Shared effective-policy resolution pipeline for run and introspection commands.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::Config;
use crate::context::Context;
use crate::env;
use crate::error::{Error, Result};
use crate::filter::{self, RunContext};
use crate::home::{self, EffectiveHome};
use crate::profile::{self, Access, LayerRegistry, MatchKind, PathGrant, Profile};
use crate::sandbox::Spec;

/// How a layer entered the resolved stack (for `--show-policies` provenance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerOrigin {
    /// Named via `--profile` (or config `default_profiles`).
    Explicit,
    /// Pulled in by `--auto-profiles` executable matching.
    Auto,
    /// Dragged in transitively via another layer's `requires`.
    Required,
}

impl LayerOrigin {
    /// A lowercase label for this provenance (`explicit` / `auto` / `required`).
    pub fn label(self) -> &'static str {
        match self {
            LayerOrigin::Explicit => "explicit",
            LayerOrigin::Auto => "auto",
            LayerOrigin::Required => "required",
        }
    }
}

/// Fully resolved policy for a confined command.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct EffectivePolicy {
    /// Resolved layer stack in merge order (deps-first), tagged with provenance.
    pub layer_names: Vec<(String, LayerOrigin)>,
    /// The merged, deny-first profile with all path grants resolved.
    pub profile: Profile,
    /// The sanitized environment for the confined process.
    pub env: std::collections::HashMap<String, String>,
    /// The effective `$HOME` (and its seed list) for the run.
    pub home: EffectiveHome,
    /// The command after profile `rewrite` rules are applied (what actually runs).
    pub cmd: Vec<String>,
    /// Applied recipe strategies (`id`, strategy label).
    pub recipes: Vec<(String, String)>,
}

/// Build a [`Spec`] from a loaded [`Config`], applying the documented precedence
/// chain ([`_docs/config.md`](../../_docs/config.md) §7).
///
/// Order, lowest to highest: builtin defaults → config file (already merged with
/// any project marker overlay by [`crate::config::load`]) → `ISOL8_*` overrides
/// (apply [`crate::config::apply_env_overrides`] to `cfg` first) → **fields the
/// caller pre-set on `base`** → the selected cage, which fills what is still
/// empty.
///
/// Pass `Spec::default()` as `base` for "exactly what the config says"; pass a
/// partially-filled `Spec` to model CLI flags, which win over the config.
///
/// ```no_run
/// let ctx = isol8_core::Context::from_environment()?;
/// let mut cfg = isol8_core::config::load()?;
/// isol8_core::config::apply_env_overrides(&mut cfg);
/// let spec = isol8_core::resolve::spec_from_config(
///     &cfg,
///     isol8_core::Spec::default(),
///     vec!["echo".into(), "hi".into()],
///     &ctx,
/// )?;
/// # Ok::<(), isol8_core::Error>(())
/// ```
pub fn spec_from_config(cfg: &Config, base: Spec, cmd: Vec<String>, ctx: &Context) -> Result<Spec> {
    let mut spec = base;
    spec.cmd = cmd;

    // Cage first: it is the more specific selection, so it wins over the global
    // config for anything the caller did not set explicitly.
    let name = crate::cage::select_name(None, cfg);
    if let Some(cage) = crate::cage::resolve_in(name.as_deref(), &ctx.cwd, Some(&ctx.config_dir))? {
        crate::cage::apply_overlay(&cage.overlay(), &mut spec);
    }

    if spec.profiles.is_empty() {
        spec.profiles = cfg.default_profiles.clone();
    }
    if spec.profile_paths.is_empty() {
        spec.profile_paths = cfg.profile_paths.clone();
    }
    if spec.add_dirs_rw.is_empty() {
        spec.add_dirs_rw = cfg.add_dirs_rw.clone();
    }
    if spec.add_dirs_ro.is_empty() {
        spec.add_dirs_ro = cfg.add_dirs_ro.clone();
    }
    if spec.home.is_none() {
        spec.home = cfg.home.clone();
    }
    if !spec.auto_profiles {
        spec.auto_profiles = cfg.auto_profiles;
    }
    Ok(spec)
}

/// Resolve layer stack, home, merged profile, and env without spawning.
///
/// Reads ambient state from the process environment. In-process hosts and
/// hermetic tests should use [`effective_policy_in`] with an explicit
/// [`Context`] instead.
pub fn effective_policy(spec: &Spec) -> Result<EffectivePolicy> {
    let ambient = Context::from_environment()?;
    effective_policy_in(spec, &ambient)
}

/// [`effective_policy`] against an explicit [`Context`].
///
/// Nothing in this path reads `HOME`, the cwd, or `ISOL8_CONFIG_PATH` behind the
/// caller's back — every ambient value comes from `ambient`. This is the entry
/// point for an embedder whose process environment belongs to the host.
pub fn effective_policy_in(spec: &Spec, ambient: &Context) -> Result<EffectivePolicy> {
    let registry = LayerRegistry::load(&spec.profile_paths)?;
    let ctx = RunContext::from_cmd(&spec.cmd);

    // Recipes compile first: their `requires` join the layer selection (a recipe
    // that needs `integrations/git` must get it) and their home ops must enter
    // the plan before the home is resolved.
    let recipe_reg = crate::recipe::RecipeRegistry::load(&spec.recipe_paths)?;
    let contributions = recipe_reg.compile_all(&spec.toolchains, &ctx)?;
    let recipes: Vec<(String, String)> = contributions
        .iter()
        .map(|c| (c.id.clone(), c.strategy.as_str().to_string()))
        .collect();

    let mut selected = profile::select_layer_names(spec, &registry, &ctx)?;
    // Provenance is judged against the pre-recipe selection, so a layer a recipe
    // dragged in reads as `required`, not as an auto-match.
    let selected_set: std::collections::HashSet<String> = selected.iter().cloned().collect();
    for c in &contributions {
        for req in &c.requires {
            if !selected.iter().any(|s| s == req) {
                selected.push(req.clone());
            }
        }
    }
    let all = registry.profiles();
    let resolved = profile::resolve_requires(&selected, &all)?;

    // Classify provenance: explicit (named) > auto (selected but not named) > required.
    let explicit: std::collections::HashSet<&str> =
        spec.profiles.iter().map(String::as_str).collect();
    let layer_names: Vec<(String, LayerOrigin)> = resolved
        .iter()
        .map(|(name, _)| {
            let origin = if explicit.contains(name.as_str()) {
                LayerOrigin::Explicit
            } else if selected_set.contains(name) {
                LayerOrigin::Auto
            } else {
                LayerOrigin::Required
            };
            (name.clone(), origin)
        })
        .collect();

    // Apply each layer's own `filter` / `[[policies]]` before anything reads the
    // layers. A non-matching layer keeps its shell (so ordering and provenance
    // are unchanged) but contributes no grants, env, home policy or rewrite.
    // This must happen here, in the one pipeline every caller uses — running it
    // in a helper that only some callers reach is how it silently stopped
    // applying at all.
    let layers: Vec<Profile> = resolved
        .into_iter()
        .map(|(_, p)| filter::apply_layer_filter(p, &ctx))
        .collect();

    let mut spec_for_home = spec.clone();
    for c in &contributions {
        spec_for_home.home_ops.extend(c.home_ops.iter().cloned());
    }

    let effective_home = home::resolve(&spec_for_home, &layers, ambient)?;
    let mut merged = profile::load_merged(spec, &layers, &effective_home, ambient)?;

    // Fold recipe path grants + env (token-expand env against Context + effective home).
    for c in &contributions {
        for g in &c.paths {
            merged.paths.push(PathGrant {
                path: home::expand_grant_in(
                    &g.path,
                    &effective_home.path,
                    &effective_home.real_home,
                ),
                access: g.access,
                r#match: g.r#match,
            });
        }
        for (k, v) in &c.env {
            let expanded =
                crate::recipe::expand_env_value(v, &effective_home.real_home, &effective_home.path);
            // Recipe env is a default (like profile env): do not clobber earlier keys.
            merged.env.entry(k.clone()).or_insert(expanded);
        }
    }

    let set_env = parse_set_env(&spec.set_env)?;
    let mut env_map = env::build_minimal(&merged, &effective_home.path, &spec.env_pass, &set_env);
    apply_path_prepend(&mut env_map, &contributions, &effective_home)?;
    let cmd = profile::apply_rewrite(&spec.cmd, &merged.rewrite);
    Ok(EffectivePolicy {
        layer_names,
        profile: merged,
        env: env_map,
        home: effective_home,
        cmd,
        recipes,
    })
}

/// Prepend recipe `path_prepend` directories to `PATH`, in recipe order.
///
/// Runs last, after `env::build_minimal` and `--set-env`: `PATH` is a single
/// scalar, so a recipe cannot contribute to it through the first-writer-wins env
/// merge, yet version managers resolve entirely through their shim directories.
/// Existing entries are kept (deduplicated, first occurrence wins) — this widens
/// lookup, never confinement: a directory on `PATH` is still unreachable unless
/// some layer or recipe granted it.
fn apply_path_prepend(
    env: &mut std::collections::HashMap<String, String>,
    contributions: &[crate::recipe::RecipeContribution],
    home: &EffectiveHome,
) -> Result<()> {
    // Links the plan will create, so a glob under a not-yet-materialized symlink
    // still resolves (cold home = warm home).
    let links: Vec<(PathBuf, PathBuf)> = home
        .plan
        .ops
        .iter()
        .filter(|op| matches!(op.kind, crate::plan::HomeOpKind::Link))
        .filter_map(|op| Some((op.to.clone()?, op.from.clone()?)))
        .collect();
    let dirs: Vec<String> = contributions
        .iter()
        .flat_map(|c| c.path_prepend.iter())
        .flat_map(|raw| {
            crate::recipe::expand_path_prepend(raw, &home.real_home, &home.path, &links)
        })
        .collect();
    if dirs.is_empty() {
        return Ok(());
    }
    let current = env.get("PATH").cloned().unwrap_or_default();
    let mut parts: Vec<PathBuf> = dirs.into_iter().map(PathBuf::from).collect();
    parts.extend(std::env::split_paths(&current));
    let mut seen = std::collections::HashSet::new();
    parts.retain(|p| !p.as_os_str().is_empty() && seen.insert(p.clone()));
    let joined = std::env::join_paths(&parts).map_err(|e| {
        Error::InvalidEnv(format!(
            "recipe path_prepend produced an unusable PATH entry: {e}"
        ))
    })?;
    env.insert("PATH".into(), joined.to_string_lossy().into_owned());
    Ok(())
}

/// Parse `--set-env K=V` entries into pairs. Errors (no silent loss) on a missing
/// `=` or an empty key.
///
/// Public because [`crate::env::build_minimal`] takes the parsed pairs — without
/// this, an embedder calling it directly would have to reimplement the split.
pub fn parse_set_env(raw: &[String]) -> Result<Vec<(String, String)>> {
    raw.iter()
        .map(|e| match e.split_once('=') {
            Some((k, v)) if !k.is_empty() => Ok((k.to_string(), v.to_string())),
            _ => Err(Error::InvalidEnv(format!(
                "--set-env {e:?} (expected NAME=VALUE)"
            ))),
        })
        .collect()
}

/// Prepare a resolved policy for actual execution (run / `@diag`): resolve `cmd[0]`
/// to an absolute path the way execvp would (host `PATH` search) and auto-grant
/// read+exec on the resolved binary. This surfaces a clear "command not found"
/// here rather than as an opaque sandbox-exec failure, makes the lookup independent
/// of the sanitized in-sandbox PATH, and ensures the command's own executable is
/// reachable under deny-by-default even when it lives outside the granted trees
/// (e.g. `~/.local/bin/<agent>`). Introspection paths leave the command unchanged.
pub fn confine_executable(profile: &mut Profile, cmd: &mut [String]) -> Result<()> {
    if let Some(first) = cmd.first() {
        let exe = resolve_executable(first)?;
        let exe_str = exe.to_string_lossy().into_owned();
        profile.paths.push(PathGrant {
            path: exe_str.clone(),
            access: Access::Ro,
            r#match: MatchKind::Literal,
        });
        // Node-based agents are launched via a thin script (often a PATH symlink)
        // that `require.resolve`s sibling/nested packages at runtime. Granting only
        // the entry script hides those, so resolution fails (e.g. codex's platform
        // binary under node_modules/@openai/codex-darwin-arm64). Grant the enclosing
        // node package directory ro so the whole package — incl. nested node_modules
        // — is reachable. No-op for non-node binaries.
        if let Some(pkg) = node_package_dir(&exe) {
            profile.paths.push(PathGrant {
                path: pkg.to_string_lossy().into_owned(),
                access: Access::Ro,
                r#match: MatchKind::Subpath,
            });
        }
        cmd[0] = exe_str;
    }
    Ok(())
}

/// If `exe` (after symlink resolution) lives inside a `node_modules` tree, return
/// the enclosing package directory: `node_modules/<pkg>` or `node_modules/@scope/<pkg>`.
/// Uses the *last* `node_modules` segment so the innermost package wins.
fn node_package_dir(exe: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(exe).ok()?;
    let comps: Vec<&std::ffi::OsStr> = real.iter().collect();
    let nm = comps.iter().rposition(|c| *c == "node_modules")?;
    // First path component after node_modules; if it's a scope (`@...`), include the next.
    let first = comps.get(nm + 1)?;
    let last = if first.to_string_lossy().starts_with('@') {
        nm + 2
    } else {
        nm + 1
    };
    if last >= comps.len() {
        return None;
    }
    Some(comps[..=last].iter().collect())
}

/// Resolve `name` to an absolute executable path, mirroring execvp: a name
/// containing `/` is treated as a path (relative to cwd), otherwise the host
/// `PATH` is searched. Returns a clean error if nothing executable is found.
fn looks_like_path(name: &str) -> bool {
    let p = Path::new(name);
    p.has_root() || name.contains('/') || name.contains('\\')
}

fn resolve_executable(name: &str) -> Result<PathBuf> {
    if looks_like_path(name) {
        let p = Path::new(name);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()?.join(p)
        };
        if is_executable_file(&abs) {
            return Ok(abs);
        }
        return Err(Error::CommandNotFound(name.to_string()));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let cand = dir.join(name);
        if is_executable_file(&cand) {
            return Ok(cand);
        }
    }
    Err(Error::CommandNotFound(name.to_string()))
}

/// True if `p` exists, is a regular file (symlinks followed), and is executable.
#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

#[cfg(windows)]
fn is_executable_file(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(m) => {
            m.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| matches!(e, "exe" | "bat" | "cmd" | "ps1"))
                    .unwrap_or(false)
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn resolves_path_command_to_absolute() {
        // /bin/sh exists and is executable on every unix host; point PATH at it so
        // the bare-name branch resolves regardless of the test runner's own PATH.
        std::env::set_var("PATH", "/bin");
        let p = resolve_executable("sh").unwrap();
        assert_eq!(p, PathBuf::from("/bin/sh"));
        assert!(is_executable_file(&p));
    }

    #[cfg(unix)]
    #[test]
    fn absolute_path_passes_through() {
        assert_eq!(
            resolve_executable("/bin/sh").unwrap(),
            PathBuf::from("/bin/sh")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_absolute_path_with_backslashes() {
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".into());
        let cmd = format!("{system_root}\\System32\\cmd.exe");
        if is_executable_file(Path::new(&cmd)) {
            assert_eq!(resolve_executable(&cmd).unwrap(), PathBuf::from(&cmd));
        }
    }

    #[test]
    fn node_package_dir_finds_scoped_and_plain_packages() {
        let raw = std::env::temp_dir().join(format!("isol8-nm-{}", std::process::id()));
        std::fs::create_dir_all(&raw).unwrap();
        // canonicalize so the expected paths match symlinked tmp (e.g. macOS /var→/private/var).
        let base = std::fs::canonicalize(&raw).unwrap();
        // scoped: node_modules/@openai/codex/bin/codex.js
        let scoped = base.join("node_modules/@openai/codex/bin");
        std::fs::create_dir_all(&scoped).unwrap();
        let js = scoped.join("codex.js");
        std::fs::write(&js, "//").unwrap();
        assert_eq!(
            node_package_dir(&js),
            Some(base.join("node_modules/@openai/codex"))
        );
        // plain: node_modules/foo/lib/cli.js
        let plain = base.join("node_modules/foo/lib");
        std::fs::create_dir_all(&plain).unwrap();
        let cli = plain.join("cli.js");
        std::fs::write(&cli, "//").unwrap();
        assert_eq!(node_package_dir(&cli), Some(base.join("node_modules/foo")));
        // non-node path: no package dir.
        assert_eq!(node_package_dir(Path::new("/bin/sh")), None);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn parse_set_env_pairs_and_errors() {
        let ok = parse_set_env(&["FOO=bar".into(), "EMPTY=".into()]).unwrap();
        assert_eq!(
            ok,
            vec![("FOO".into(), "bar".into()), ("EMPTY".into(), "".into())]
        );
        // a `=` in the value is fine (split on the first only).
        let v = parse_set_env(&["URL=a=b".into()]).unwrap();
        assert_eq!(v, vec![("URL".to_string(), "a=b".to_string())]);
        // no `=` or empty key → error, not silent drop.
        assert!(parse_set_env(&["NOEQ".into()]).is_err());
        assert!(parse_set_env(&["=val".into()]).is_err());
    }

    #[test]
    fn missing_command_is_clean_error() {
        let err = resolve_executable("definitely-not-a-real-cmd-xyz").unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"command "definitely-not-a-real-cmd-xyz" not found"#
        );
    }
}
