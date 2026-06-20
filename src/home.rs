//! R4 — effective-home resolution and seeding. The effective home is resolved
//! *before* any path-grant computation so every `~`-relative grant targets the
//! replacement home, not the real one (profile-model §7).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::cli::RunArgs;
use crate::profile::Profile;

/// The resolved effective home for a run.
pub struct EffectiveHome {
    pub path: PathBuf,
    /// Real-home entries to seed read-only into the home (e.g. "~/.gitconfig").
    pub seed: Vec<String>,
}

/// Resolve the effective home with precedence: `--home` > layer `home_replace.path`
/// > `auto_scratch` temp dir.
///
/// `layers` are the resolved (deps-first) layers; the highest layer that sets a
/// `home_replace` wins, matching merge semantics. Seeds are unioned across layers.
///
/// ponytail: std scratch dir, no tempfile crate; cleanup best-effort.
pub fn resolve(run: &RunArgs, layers: &[Profile]) -> Result<EffectiveHome> {
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

    let real = real_home();
    let path = if let Some(home) = run.home() {
        PathBuf::from(expand_tilde(home, &real))
    } else if let Some(p) = hr_path {
        PathBuf::from(expand_tilde(&p, &real))
    } else if auto_scratch {
        create_scratch_home()?
    } else {
        // No replacement requested: fall back to the real home.
        real
    };

    Ok(EffectiveHome { path, seed })
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
                    .with_context(|| format!("stat scratch home {}", dir.display()))?;
                if meta.file_type().is_symlink() {
                    continue;
                }
                return Ok(dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("creating scratch home at {}", dir.display()));
            }
        }
    }
    anyhow::bail!("failed to create a unique scratch home directory after 16 attempts");
}

/// The real `$HOME`, or `/` if unset (never panics on user environment).
fn real_home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
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

/// Copy allowlisted real-home entries read-only into the (scratch) home (R4.4).
///
/// Keeps it simple: std fs copy of files, recursive copy of directories. Missing
/// sources are skipped (best-effort seeding); copied files are made read-only.
pub fn seed(home: &EffectiveHome) -> Result<()> {
    let real = real_home();
    for entry in &home.seed {
        // Seed entries are real-home-relative `~/...` references.
        let rel = entry.strip_prefix("~/").unwrap_or(entry);
        let src = real.join(rel);
        if !src.exists() {
            continue; // best-effort
        }
        let dst = home.path.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        copy_readonly(&src, &dst)
            .with_context(|| format!("seeding {} -> {}", src.display(), dst.display()))?;
    }
    Ok(())
}

/// Recursively copy `src` to `dst`, marking copied files read-only.
fn copy_readonly(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_readonly(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst)?;
        let mut perms = std::fs::metadata(dst)?.permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(dst, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_root() {
        assert_eq!(expand_tilde("~", Path::new("/scratch")), "/scratch");
    }

    #[test]
    fn expand_tilde_subpath() {
        assert_eq!(
            expand_tilde("~/.cargo", Path::new("/scratch")),
            "/scratch/.cargo"
        );
    }

    #[test]
    fn expand_tilde_passthrough() {
        assert_eq!(expand_tilde("/usr/bin", Path::new("/scratch")), "/usr/bin");
        // mid-string tilde is not a home reference
        assert_eq!(expand_tilde("/a/~/b", Path::new("/scratch")), "/a/~/b");
    }

    #[test]
    fn resolve_expands_tilde_in_cli_home() {
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", "/real/home");

        let run = crate::cli::run_from(
            crate::cli::ProfileOpts {
                home: Some("~/scratch".into()),
                ..Default::default()
            },
            vec!["echo".into()],
        );
        let home = resolve(&run, &[]).unwrap();
        assert_eq!(home.path, PathBuf::from("/real/home/scratch"));

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn resolve_honors_home_replace_enabled_false() {
        let run = crate::cli::run_from(Default::default(), vec!["echo".into()]);
        let layers = vec![crate::profile::Profile {
            home_replace: Some(crate::profile::HomeReplace {
                enabled: false,
                auto_scratch: true,
                path: None,
                seed: vec![],
            }),
            ..Default::default()
        }];
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", "/real/home");

        let home = resolve(&run, &layers).unwrap();
        assert_eq!(home.path, PathBuf::from("/real/home"));

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn scratch_home_paths_are_unique() {
        let run = crate::cli::run_from(Default::default(), vec!["echo".into()]);
        let layers = vec![crate::profile::Profile {
            home_replace: Some(crate::profile::HomeReplace {
                enabled: true,
                auto_scratch: true,
                path: None,
                seed: vec![],
            }),
            ..Default::default()
        }];
        let a = resolve(&run, &layers).unwrap().path;
        let b = resolve(&run, &layers).unwrap().path;
        assert_ne!(a, b);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn seed_copies_readonly() {
        let tmp = std::env::temp_dir().join(format!("isol8-test-seed-{}", std::process::id()));
        let real = tmp.join("real");
        let scratch = tmp.join("scratch");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join(".gitconfig"), b"x").unwrap();

        // Point HOME at our fake real home for the duration of this test.
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", &real);

        let home = EffectiveHome {
            path: scratch.clone(),
            seed: vec!["~/.gitconfig".into()],
        };
        seed(&home).unwrap();

        let copied = scratch.join(".gitconfig");
        assert!(copied.exists());
        assert!(std::fs::metadata(&copied).unwrap().permissions().readonly());

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
