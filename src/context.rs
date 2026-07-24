//! Injectable ambient context for path token expansion and home resolution.
//!
//! Tokens like `~`, `#HOME`, and `@managed/<id>` are not paths until resolved
//! against a [`Context`]. The CLI builds one via [`Context::from_environment`];
//! tests inject hermetic values. See `_docs/wip/multi-evo-plan.md` Phase 2 and
//! evo-repo §7.4.

use std::path::PathBuf;

use crate::error::{Error, Result};

/// Platform label used for filter matching and managed-home layout notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Ambient state for token expansion and managed homes.
///
/// Never read environment variables behind the host's back once constructed —
/// build with [`Context::from_environment`] (CLI) or a test fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// The user's real `$HOME` (`#HOME` token).
    pub real_home: PathBuf,
    /// Process working directory at resolve time.
    pub cwd: PathBuf,
    /// Host platform.
    pub platform: Platform,
    /// Base directory for `@managed/<id>` homes.
    pub managed_root: PathBuf,
}

impl Context {
    /// Build from the process environment (CLI entry point).
    pub fn from_environment() -> Result<Self> {
        let real_home = real_home_from_env();
        let cwd = std::env::current_dir()
            .map_err(|e| Error::Message(format!("cannot determine current directory: {e}")))?;
        let managed_root = default_managed_root(&real_home);
        Ok(Self {
            real_home,
            cwd,
            platform: Platform::current(),
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
        Ok(self.managed_root.join(id))
    }
}

/// Default managed-homes root for the platform.
///
/// - Unix: `$XDG_DATA_HOME/isol8/homes` or `~/.local/share/isol8/homes`
/// - Windows: `%LOCALAPPDATA%\\isol8\\homes` (fallback: real_home\\AppData\\Local\\isol8\\homes)
pub fn default_managed_root(real_home: &std::path::Path) -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| real_home.join("AppData").join("Local"))
            .join("isol8")
            .join("homes")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| real_home.join(".local").join("share"))
            .join("isol8")
            .join("homes")
    }
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

    #[test]
    fn managed_home_rejects_traversal() {
        let ctx = Context {
            real_home: PathBuf::from("/home/u"),
            cwd: PathBuf::from("/tmp"),
            platform: Platform::Linux,
            managed_root: PathBuf::from("/data/isol8/homes"),
        };
        assert!(ctx.managed_home("work").is_ok());
        assert!(ctx.managed_home("../x").is_err());
        assert!(ctx.managed_home("a/b").is_err());
        assert!(ctx.managed_home("").is_err());
    }

    #[test]
    fn managed_home_joins_root() {
        let ctx = Context {
            real_home: PathBuf::from("/home/u"),
            cwd: PathBuf::from("/tmp"),
            platform: Platform::Macos,
            managed_root: PathBuf::from("/data/isol8/homes"),
        };
        assert_eq!(
            ctx.managed_home("work").unwrap(),
            PathBuf::from("/data/isol8/homes/work")
        );
    }
}
