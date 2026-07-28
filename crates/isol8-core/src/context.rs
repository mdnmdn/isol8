//! Injectable ambient context for path token expansion and home resolution.
//!
//! Tokens like `~`, `#HOME`, `@…` (config-relative), and `@managed/<id>` are not
//! paths until resolved against a [`Context`]. The CLI builds one via
//! [`Context::from_environment`]; tests inject hermetic values.
//!
//! **`@` paths** resolve under the **effective config directory** (project
//! `.isol8.toml` `config_path`, `ISOL8_CONFIG_PATH`, or `~/.config/isol8`).
//! **`@managed/<id>`** is `{config_dir}/homes/<id>`.
//!
//! See `_docs/wip/multi-evo-plan.md` Phase 2 and evo-repo §7.4.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};

/// Platform label used for filter matching and managed-home layout notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Apple macOS.
    Macos,
    /// Linux (including WSL2).
    Linux,
    /// Microsoft Windows.
    Windows,
    /// Anything else (best-effort).
    Other,
}

impl Platform {
    /// Detect from `std::env::consts::OS`.
    pub fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Platform::Macos,
            "linux" => Platform::Linux,
            "windows" => Platform::Windows,
            _ => Platform::Other,
        }
    }

    /// Lowercase name matching profile `filter.os` values.
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Macos => "macos",
            Platform::Linux => "linux",
            Platform::Windows => "windows",
            Platform::Other => "other",
        }
    }
}

/// Make `path` absolute and lexically normalized (`.` / `..` collapsed).
///
/// Relative paths are joined with the process cwd at call time. Does **not**
/// require the path to exist (`canonicalize` would fail for not-yet-created
/// `@managed` homes).
pub fn absolute_path(path: &Path) -> PathBuf {
    let abs = if path.as_os_str().is_empty() {
        match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(_) => PathBuf::from("."),
        }
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    normalize_lexically(&abs)
}

/// Collapse `.` and `..` without touching the filesystem.
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// Ambient state for token expansion and managed homes.
///
/// Never read environment variables behind the host's back once constructed —
/// build with [`Context::from_environment`] (CLI) or a test fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Context {
    /// The user's real `$HOME` (`#HOME` token).
    pub real_home: PathBuf,
    /// Process working directory at resolve time.
    pub cwd: PathBuf,
    /// Host platform.
    pub platform: Platform,
    /// Effective isol8 config root (`@…` path expansion base).
    pub config_dir: PathBuf,
    /// Base directory for `@managed/<id>` homes (`{config_dir}/homes`).
    pub managed_root: PathBuf,
}

impl Context {
    /// Build from the process environment (CLI entry point).
    ///
    /// The config root follows [`crate::config::effective_config_dir`], so a
    /// project `config_path` / `ISOL8_CONFIG_PATH` redirects `@` and `@managed`.
    pub fn from_environment() -> Result<Self> {
        let real_home = absolute_path(&real_home_from_env());
        let cwd = std::env::current_dir()
            .map_err(|e| Error::Message(format!("cannot determine current directory: {e}")))?;
        let config_dir = crate::config::effective_config_dir(); // already absolute
        let managed_root = managed_root_for_config(&config_dir);
        Ok(Self {
            real_home,
            cwd: absolute_path(&cwd),
            platform: Platform::current(),
            config_dir,
            managed_root,
        })
    }

    /// Resolve `@managed/<id>` to an absolute path under [`Context::managed_root`].
    pub fn managed_home(&self, id: &str) -> Result<PathBuf> {
        let id = id.trim();
        if id.is_empty() || id.contains('/') || id.contains('\\') || id.contains("..") || id == "."
        {
            return Err(Error::Message(format!(
                "invalid managed home id {id:?} (expected a single path segment)"
            )));
        }
        Ok(absolute_path(&self.managed_root.join(id)))
    }

    /// Expand a path that starts with `@` relative to [`Context::config_dir`].
    ///
    /// - `@managed/<id>` is **not** handled here (use [`Self::managed_home`]).
    /// - `@/profiles` or `@profiles` → `{config_dir}/profiles`
    /// - `@` alone → `config_dir`
    ///
    /// Returns `None` when `path` does not start with `@`.
    pub fn expand_at(&self, path: &str) -> Option<PathBuf> {
        expand_at_path(path, &self.config_dir)
    }

    /// Human description of a cage/CLI `home` value with the effective path.
    pub fn describe_home(&self, home: &str) -> Result<String> {
        match home.trim() {
            "" | "inherit" => Ok(format!(
                "inherit → {} (real $HOME)",
                self.real_home.display()
            )),
            "ephemeral" => Ok("ephemeral → (fresh temp dir each run)".into()),
            other if other.starts_with("@managed/") => {
                let id = other.trim_start_matches("@managed/");
                let path = self.managed_home(id)?;
                let note = if path.is_dir() {
                    "exists"
                } else {
                    "created on first run"
                };
                Ok(format!("@managed/{id} → {} ({note})", path.display()))
            }
            other if other.starts_with('@') => {
                let path = self
                    .expand_at(other)
                    .unwrap_or_else(|| PathBuf::from(other));
                Ok(format!("{other} → {}", path.display()))
            }
            other => {
                let expanded = crate::home::expand_tilde(other, &self.real_home);
                if expanded == other {
                    Ok(other.to_string())
                } else {
                    Ok(format!("{other} → {expanded}"))
                }
            }
        }
    }
}

/// `{config_dir}/homes` — location of `@managed/<id>` directories (absolute).
pub fn managed_root_for_config(config_dir: &Path) -> PathBuf {
    absolute_path(&config_dir.join("homes"))
}

/// Expand `@…` relative to `config_dir`. Non-`@` paths return `None`.
///
/// Result is always absolute. Does **not** special-case `@managed/` — callers
/// that need managed homes should strip that prefix first.
pub fn expand_at_path(path: &str, config_dir: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix('@')?;
    let rest = rest
        .strip_prefix('/')
        .or_else(|| rest.strip_prefix('\\'))
        .unwrap_or(rest);
    let joined = if rest.is_empty() {
        config_dir.to_path_buf()
    } else {
        config_dir.join(rest)
    };
    Some(absolute_path(&joined))
}

/// OS default isol8 config directory (`~/.config/isol8`, `$XDG_CONFIG_HOME/isol8`, …).
pub fn default_os_config_dir(real_home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            if !real_home.as_os_str().is_empty() {
                Some(real_home.join(".config"))
            } else {
                None
            }
        })
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("APPDATA")
                    .filter(|h| !h.is_empty())
                    .map(PathBuf::from)
            } else {
                None
            }
        })
        .unwrap_or_else(|| real_home.join(".config"))
        .join("isol8")
}

/// Default managed-homes root when only the real home is known (no config redirect).
///
/// Prefer [`managed_root_for_config`] with the effective config dir.
pub fn default_managed_root(real_home: &Path) -> PathBuf {
    managed_root_for_config(&default_os_config_dir(real_home))
}

/// Real `$HOME` / `USERPROFILE` with platform fallbacks (never panics).
pub fn real_home_from_env() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("USERPROFILE")
                    .filter(|h| !h.is_empty())
                    .map(PathBuf::from)
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            if cfg!(windows) {
                PathBuf::from(r"C:\")
            } else {
                PathBuf::from("/")
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context {
            real_home: PathBuf::from("/Users/alice"),
            cwd: PathBuf::from("/tmp"),
            platform: Platform::current(),
            config_dir: PathBuf::from("/cfg/isol8"),
            managed_root: PathBuf::from("/cfg/isol8/homes"),
        }
    }

    #[test]
    fn describe_home_managed_under_config() {
        let ctx = test_ctx();
        let d = ctx.describe_home("@managed/work").unwrap();
        assert!(d.contains("@managed/work"), "{d}");
        assert!(d.contains("/cfg/isol8/homes/work"), "{d}");
        assert!(
            d.contains("created on first run") || d.contains("exists"),
            "{d}"
        );

        let i = ctx.describe_home("inherit").unwrap();
        assert!(i.contains("/Users/alice"), "{i}");
        assert!(ctx.describe_home("ephemeral").unwrap().contains("temp"));
    }

    #[test]
    fn expand_at_relative_to_config_dir() {
        let ctx = test_ctx();
        assert_eq!(
            ctx.expand_at("@/profiles").unwrap(),
            PathBuf::from("/cfg/isol8/profiles")
        );
        assert_eq!(
            ctx.expand_at("@profiles").unwrap(),
            PathBuf::from("/cfg/isol8/profiles")
        );
        assert_eq!(ctx.expand_at("@").unwrap(), PathBuf::from("/cfg/isol8"));
        assert!(ctx.expand_at("/abs").is_none());
        assert!(ctx.expand_at("rel").is_none());
    }

    #[test]
    fn managed_home_rejects_traversal() {
        let ctx = test_ctx();
        assert!(ctx.managed_home("work").is_ok());
        assert!(ctx.managed_home("../x").is_err());
        assert!(ctx.managed_home("a/b").is_err());
        assert!(ctx.managed_home("").is_err());
    }

    #[test]
    fn managed_home_joins_root() {
        let ctx = test_ctx();
        assert_eq!(
            ctx.managed_home("work").unwrap(),
            PathBuf::from("/cfg/isol8/homes/work")
        );
    }

    #[test]
    fn managed_root_for_config_is_homes_subdir() {
        assert_eq!(
            managed_root_for_config(Path::new("/data/config")),
            PathBuf::from("/data/config/homes")
        );
    }

    #[test]
    fn absolute_path_joins_cwd_and_normalizes() {
        let cwd = std::env::current_dir().unwrap();
        let abs = absolute_path(Path::new("./foo/../bar"));
        assert!(abs.is_absolute(), "{abs:?}");
        assert_eq!(abs, normalize_lexically(&cwd.join("bar")));
        assert_eq!(
            absolute_path(Path::new("/already/abs")),
            PathBuf::from("/already/abs")
        );
    }
}
