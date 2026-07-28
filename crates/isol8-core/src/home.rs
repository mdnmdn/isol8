//! R4 — effective-home resolution and materialization planning.
//!
//! The effective home is resolved *before* any path-grant computation so every
//! `~`-relative grant targets the replacement home, not the real one
//! (profile-model §7). Seeding and other home ops go through [`crate::plan::HomePlan`]
//! (plan then apply).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::context::{self, Context};
use crate::error::{Error, Result, ResultExt};
use crate::plan::{self, HomeOpSpec, HomePlan};
use crate::profile::Profile;
use crate::sandbox::Spec;

/// Token usable in profile/CLI path grants that expands to the *real* home, so a
/// grant survives an active `--home` replacement (which `~` would retarget). §9.
pub const REAL_HOME_TOKEN: &str = "#HOME";

/// Prefix for isol8-managed homes under [`Context::managed_root`].
pub const MANAGED_HOME_PREFIX: &str = "@managed/";

/// The resolved effective home for a run, plus its materialization plan.
pub struct EffectiveHome {
    /// The resolved effective `$HOME` directory for the run.
    pub path: PathBuf,
    /// Real home used for `#HOME` expansion (from [`Context`]).
    pub real_home: PathBuf,
    /// Real-home entries to seed (profile `home_replace.seed`); also reflected in `plan`.
    pub seed: Vec<String>,
    /// Materialization plan (seed-ro + Spec home_ops). Apply on spawn only.
    pub plan: HomePlan,
}

/// Resolve the effective home with precedence: `--home` > layer `home_replace.path`
/// > `auto_scratch` / `ephemeral_home` temp dir > real home.
///
/// `layers` are the resolved (deps-first) layers; the highest layer that sets a
/// `home_replace` wins, matching merge semantics. Seeds are unioned across layers
/// and converted into seed-ro plan ops together with [`Spec::home_ops`].
///
/// Uses [`Context`] for real home, managed-root, and token expansion — never
/// re-reads the environment for those values.
pub fn resolve(spec: &Spec, layers: &[Profile], ctx: &Context) -> Result<EffectiveHome> {
    // Highest layer that sets home_replace wins; seeds union across all layers.
    let mut hr_path: Option<String> = None;
    let mut auto_scratch = false;
    let mut seed: Vec<String> = Vec::new();
    for layer in layers {
        if let Some(hr) = &layer.home_replace {
            if !hr.enabled {
                continue;
            }
            hr_path = hr.path.clone();
            auto_scratch = hr.auto_scratch;
            for s in &hr.seed {
                if !seed.contains(s) {
                    seed.push(s.clone());
                }
            }
        }
    }

    let path = if let Some(home) = &spec.home {
        resolve_home_spec(home, ctx)?
    } else if let Some(p) = hr_path {
        resolve_home_spec(&p, ctx)?
    } else if auto_scratch || spec.ephemeral_home {
        create_scratch_home()?
    } else {
        // No replacement requested: fall back to the real home.
        ctx.real_home.clone()
    };

    if spec.no_seed {
        seed.clear();
    }

    // Build materialization specs: ensure managed home dir, then seeds, then explicit ops.
    let mut specs: Vec<HomeOpSpec> = Vec::new();
    if path != ctx.real_home {
        // Ensure the replacement home directory exists before seed/link targets.
        specs.push(HomeOpSpec::mkdir(path.to_string_lossy().into_owned()));
    }
    specs.extend(plan::seed_specs_from_list(&seed));
    specs.extend(spec.home_ops.clone());

    let plan = HomePlan::compute(&specs, ctx, &path)?;

    Ok(EffectiveHome {
        path,
        real_home: ctx.real_home.clone(),
        seed,
        plan,
    })
}

/// Resolve a home path string: `@managed/<id>`, `@…` (config-relative), `~…`, or absolute.
///
/// Always returns an absolute path so later `chdir` cannot retarget it.
fn resolve_home_spec(home: &str, ctx: &Context) -> Result<PathBuf> {
    if let Some(id) = home.strip_prefix(MANAGED_HOME_PREFIX) {
        return ctx.managed_home(id);
    }
    if let Some(p) = ctx.expand_at(home) {
        return Ok(p);
    }
    Ok(context::absolute_path(&PathBuf::from(expand_tilde(
        home,
        &ctx.real_home,
    ))))
}

/// Create a unique scratch home under the OS temp dir (not predictable from PID alone).
fn create_scratch_home() -> Result<PathBuf> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    for attempt in 0..16 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "isol8-home-{}-{}-{}-{attempt}",
            std::process::id(),
            nanos,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                let meta = std::fs::symlink_metadata(&dir)
                    .ctx(|| format!("stat scratch home {}", dir.display()))?;
                if meta.file_type().is_symlink() {
                    continue;
                }
                return Ok(dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).ctx(|| format!("creating scratch home at {}", dir.display()));
            }
        }
    }
    Err(Error::Message(
        "failed to create a unique scratch home directory after 16 attempts".into(),
    ))
}

/// The real `$HOME`, or a platform-appropriate fallback (never panics on user
/// environment). Prefer [`Context::real_home`] when a context is available.
pub fn real_home() -> PathBuf {
    context::real_home_from_env()
}

/// Expand a leading `~` / `~/...` in `path` against `home`. Non-tilde paths pass
/// through unchanged. Mid-string `~` is not expanded (only a leading segment).
pub fn expand_tilde(path: &str, home: &Path) -> String {
    if path == "~" {
        return home.to_string_lossy().into_owned();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest).to_string_lossy().into_owned();
    }
    path.to_string()
}

/// Expand a path grant: first substitute the `#HOME` real-home token (§9), then
/// expand a leading `~` against the *effective* home. With no replacement home the
/// two coincide, so `#HOME` and `~` agree.
///
/// Prefer [`expand_grant_in`] when a [`Context`] / [`EffectiveHome`] is available.
pub fn expand_grant(path: &str, effective_home: &Path) -> String {
    expand_grant_in(path, effective_home, &real_home())
}

/// Expand a path grant using an explicit real-home path (from [`Context`]).
pub fn expand_grant_in(path: &str, effective_home: &Path, real: &Path) -> String {
    let substituted = if path.contains(REAL_HOME_TOKEN) {
        path.replace(REAL_HOME_TOKEN, &real.to_string_lossy())
    } else {
        path.to_string()
    };
    let expanded = expand_tilde(&substituted, effective_home);
    #[cfg(windows)]
    {
        expand_windows_vars(&expanded)
    }
    #[cfg(not(windows))]
    {
        expanded
    }
}

/// Expand Windows `%VAR%` tokens in a path grant (e.g. `%SYSTEMROOT%`).
#[cfg(windows)]
pub fn expand_windows_vars(path: &str) -> String {
    let mut result = path.to_string();
    for (var, key) in &[
        ("%SYSTEMROOT%", "SYSTEMROOT"),
        ("%USERPROFILE%", "USERPROFILE"),
        ("%LOCALAPPDATA%", "LOCALAPPDATA"),
        ("%APPDATA%", "APPDATA"),
        ("%PROGRAMFILES%", "ProgramFiles"),
        ("%PROGRAMFILES(X86)%", "ProgramFiles(x86)"),
        ("%ALLUSERSPROFILE%", "ALLUSERSPROFILE"),
        ("%SYSTEMDRIVE%", "SYSTEMDRIVE"),
        ("%TEMP%", "TEMP"),
        ("%TMP%", "TMP"),
        ("%HOMEDRIVE%", "HOMEDRIVE"),
        ("%HOMEPATH%", "HOMEPATH"),
    ] {
        if let Some(val) = std::env::var_os(key) {
            result = result.replace(var, &val.to_string_lossy());
        }
    }
    result
}

/// Apply the home materialization plan (seed-ro, links, mkdir, copy).
///
/// Idempotent. Prefer this over ad-hoc seeding; keeps plan/apply as one path.
pub fn materialize(home: &EffectiveHome) -> Result<()> {
    home.plan.apply()
}

/// Backward-compatible alias: apply the planned seed (and other home ops).
pub fn seed(home: &EffectiveHome) -> Result<()> {
    materialize(home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Context, Platform};
    use crate::plan::HomeOpSpec;

    fn test_ctx(real: &str) -> Context {
        let config_dir = PathBuf::from(format!("{real}/.config/isol8"));
        Context {
            real_home: PathBuf::from(real),
            cwd: PathBuf::from("/tmp"),
            platform: Platform::Linux,
            managed_root: config_dir.join("homes"),
            config_dir,
        }
    }

    #[test]
    fn expand_tilde_root() {
        assert_eq!(expand_tilde("~", Path::new("/scratch")), "/scratch");
    }

    #[test]
    fn expand_tilde_subpath() {
        assert_eq!(
            expand_tilde("~/.cargo", Path::new("/scratch")),
            Path::new("/scratch").join(".cargo").to_string_lossy()
        );
    }

    #[test]
    fn expand_grant_real_home_token() {
        // `#HOME` targets the real home; `~` targets the effective (replacement) home.
        assert_eq!(
            expand_grant_in("#HOME/.ssh", Path::new("/scratch"), Path::new("/real")),
            "/real/.ssh"
        );
        assert_eq!(
            expand_grant_in("~/.cargo", Path::new("/scratch"), Path::new("/real")),
            Path::new("/scratch").join(".cargo").to_string_lossy()
        );
    }

    #[test]
    fn no_seed_clears_seed_list() {
        let run = crate::sandbox::Spec {
            no_seed: true,
            ..Default::default()
        };
        let layers = vec![crate::profile::Profile {
            home_replace: Some(crate::profile::HomeReplace {
                enabled: true,
                auto_scratch: false,
                path: Some("~/h".into()),
                seed: vec!["~/.gitconfig".into()],
            }),
            ..Default::default()
        }];
        let ctx = test_ctx("/real/home");
        let home = resolve(&run, &layers, &ctx).unwrap();
        assert!(home.seed.is_empty());
        // mkdir of replacement home may still be planned
        assert!(home
            .plan
            .ops
            .iter()
            .all(|o| o.kind != crate::plan::HomeOpKind::SeedRo));
    }

    #[cfg(windows)]
    #[test]
    fn expand_windows_vars_substitutes_systemroot() {
        std::env::set_var("SYSTEMROOT", "C:\\Windows");
        let out = expand_windows_vars("%SYSTEMROOT%\\System32");
        assert_eq!(out, "C:\\Windows\\System32");
        std::env::remove_var("SYSTEMROOT");
    }

    #[test]
    fn expand_tilde_passthrough() {
        assert_eq!(expand_tilde("/usr/bin", Path::new("/scratch")), "/usr/bin");
        // mid-string tilde is not a home reference
        assert_eq!(expand_tilde("/a/~/b", Path::new("/scratch")), "/a/~/b");
    }

    #[test]
    fn resolve_expands_tilde_in_cli_home() {
        let run = crate::sandbox::Spec {
            home: Some("~/scratch".into()),
            ..Default::default()
        };
        let ctx = test_ctx("/real/home");
        let home = resolve(&run, &[], &ctx).unwrap();
        assert_eq!(home.path, PathBuf::from("/real/home/scratch"));
    }

    #[test]
    fn resolve_managed_home() {
        let run = crate::sandbox::Spec {
            home: Some("@managed/work".into()),
            ..Default::default()
        };
        let ctx = test_ctx("/real/home");
        let home = resolve(&run, &[], &ctx).unwrap();
        assert_eq!(
            home.path,
            PathBuf::from("/real/home/.config/isol8/homes/work")
        );
    }

    #[test]
    fn resolve_ephemeral_home_flag_creates_scratch() {
        let run = crate::sandbox::Spec {
            ephemeral_home: true,
            cmd: vec!["echo".into()],
            ..Default::default()
        };
        let ctx = test_ctx("/real/home");
        let home = resolve(&run, &[], &ctx).unwrap();
        assert_ne!(home.path, PathBuf::from("/real/home"));
        assert!(
            home.path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("isol8-home-")),
            "expected scratch dir, got {}",
            home.path.display()
        );
        let _ = std::fs::remove_dir_all(&home.path);
    }

    #[test]
    fn resolve_honors_home_replace_enabled_false() {
        let run = crate::sandbox::Spec::default();
        let layers = vec![crate::profile::Profile {
            home_replace: Some(crate::profile::HomeReplace {
                enabled: false,
                auto_scratch: true,
                path: None,
                seed: vec![],
            }),
            ..Default::default()
        }];
        let ctx = test_ctx("/real/home");
        let home = resolve(&run, &layers, &ctx).unwrap();
        assert_eq!(home.path, PathBuf::from("/real/home"));
    }

    #[test]
    fn resolve_defaults_to_real_home_without_replacement() {
        let run = crate::sandbox::Spec::default();
        let ctx = test_ctx("/real/home");
        let home = resolve(&run, &[], &ctx).unwrap();
        assert_eq!(home.path, PathBuf::from("/real/home"));
        assert!(home.seed.is_empty());
    }

    #[test]
    fn resolve_uses_layer_home_replace_path() {
        let run = crate::sandbox::Spec::default();
        let layers = vec![crate::profile::Profile {
            home_replace: Some(crate::profile::HomeReplace {
                enabled: true,
                auto_scratch: false,
                path: Some("~/sandbox-home".into()),
                seed: vec!["~/.gitconfig".into()],
            }),
            ..Default::default()
        }];
        let ctx = test_ctx("/real/home");
        let home = resolve(&run, &layers, &ctx).unwrap();
        assert_eq!(home.path, PathBuf::from("/real/home/sandbox-home"));
        assert_eq!(home.seed, vec!["~/.gitconfig".to_string()]);
        assert!(home
            .plan
            .ops
            .iter()
            .any(|o| o.kind == crate::plan::HomeOpKind::SeedRo));
    }

    #[test]
    fn scratch_home_paths_are_unique() {
        let run = crate::sandbox::Spec::default();
        let layers = vec![crate::profile::Profile {
            home_replace: Some(crate::profile::HomeReplace {
                enabled: true,
                auto_scratch: true,
                path: None,
                seed: vec![],
            }),
            ..Default::default()
        }];
        let ctx = test_ctx("/real/home");
        let a = resolve(&run, &layers, &ctx).unwrap().path;
        let b = resolve(&run, &layers, &ctx).unwrap().path;
        assert_ne!(a, b);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn materialize_seed_and_ops() {
        let tmp = std::env::temp_dir().join(format!("isol8-test-mat-{}", std::process::id()));
        let real = tmp.join("real");
        let scratch = tmp.join("scratch");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join(".gitconfig"), b"x").unwrap();
        std::fs::create_dir_all(real.join(".tool")).unwrap();
        std::fs::write(real.join(".tool/bin"), b"t").unwrap();

        let ctx = Context {
            real_home: real.clone(),
            cwd: tmp.clone(),
            platform: Platform::Linux,
            config_dir: tmp.join("config"),
            managed_root: tmp.join("config/homes"),
        };
        let run = Spec {
            home: Some(scratch.to_string_lossy().into_owned()),
            home_ops: vec![HomeOpSpec::link("#HOME/.tool", "~/.tool")],
            ..Default::default()
        };
        let layers = vec![crate::profile::Profile {
            home_replace: Some(crate::profile::HomeReplace {
                enabled: true,
                auto_scratch: false,
                path: None,
                seed: vec!["~/.gitconfig".into()],
            }),
            ..Default::default()
        }];
        // home from spec wins over layer path (layer path None + enabled still sets seed)
        let home = resolve(&run, &layers, &ctx).unwrap();
        materialize(&home).unwrap();

        assert!(scratch.join(".gitconfig").exists());
        assert!(std::fs::symlink_metadata(scratch.join(".tool"))
            .unwrap()
            .file_type()
            .is_symlink());

        // Idempotent
        materialize(&home).unwrap();

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
