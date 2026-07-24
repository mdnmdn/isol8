//! Cages — named local isolation units that select profiles, home mode, and dirs.
//!
//! A cage is a **selection** layer: it compiles into fields on [`crate::sandbox::Spec`]
//! (and CLI `ProfileOpts`). It is not a profile layer and does not participate in
//! deny-first merge. See `_docs/wip/multi-evo-plan.md` Phase 1 and
//! `_docs/inbox/evo-repo.md` §3.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result, ResultExt};
use crate::profile::Access;
use crate::recipe::ToolchainChoice;

/// Current cage file schema version.
pub const CAGE_SCHEMA: u32 = 1;

/// How a cage chooses `$HOME` for the confined process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeMode {
    /// Keep the real home (no replacement). Default isol8 behaviour.
    Inherit,
    /// Fresh temporary directory per run (scratch home).
    Ephemeral,
    /// Explicit path (may contain `~`; not expanded here — `home::resolve` expands).
    Path(String),
}

impl HomeMode {
    /// Parse a cage `home = "…"` value (`inherit` | `ephemeral` | `@managed/<id>` | path).
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim() {
            "" | "inherit" => Ok(HomeMode::Inherit),
            "ephemeral" => Ok(HomeMode::Ephemeral),
            other if other.starts_with("@managed/") => {
                let id = other.trim_start_matches("@managed/");
                if id.is_empty() {
                    return Err(Error::Message(
                        "cage home `@managed/` requires an id (e.g. @managed/work)".into(),
                    ));
                }
                // Validate id early via a throwaway Context-shaped check.
                if id.contains('/') || id.contains('\\') || id.contains("..") {
                    return Err(Error::Message(format!(
                        "invalid managed home id {id:?} (expected a single path segment)"
                    )));
                }
                Ok(HomeMode::Path(format!("@managed/{id}")))
            }
            other => Ok(HomeMode::Path(other.to_string())),
        }
    }
}

/// One path grant contributed by a cage's `[[dirs]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CageDir {
    /// Path (may use `~` / `#HOME` tokens; expanded later with the rest of the policy).
    pub path: String,
    /// Only `ro` or `rw` are valid in a cage file.
    pub access: Access,
}

/// A loaded cage document plus the path it was loaded from.
#[derive(Debug, Clone)]
pub struct Cage {
    /// Schema version from the file (`schema = 1`).
    pub schema: u32,
    /// Cage name (from the file or derived from the filename stem).
    pub name: String,
    /// Home selection for this cage.
    pub home: HomeMode,
    /// Profile layer ids to enable (replaces config `default_profiles` when applied).
    pub profiles: Vec<String>,
    /// Extra path grants from `[[dirs]]`.
    pub dirs: Vec<CageDir>,
    /// Per-toolchain strategy choices (`[toolchains.<id>] strategy = "…"`).
    pub toolchains: Vec<ToolchainChoice>,
    /// Absolute path of the source file.
    pub source: PathBuf,
}

/// Overlay of Spec/CLI fields produced by a cage (no ambient merge).
#[derive(Debug, Clone, Default)]
pub struct CageOverlay {
    /// Profiles to set when the caller has none.
    pub profiles: Vec<String>,
    /// Replacement home path when [`HomeMode::Path`]; `None` for inherit/ephemeral.
    pub home: Option<String>,
    /// When true, use a scratch home (unless caller already set `home`).
    pub ephemeral_home: bool,
    /// Read-write dirs.
    pub add_dirs_rw: Vec<String>,
    /// Read-only dirs.
    pub add_dirs_ro: Vec<String>,
    /// Toolchain recipe choices from the cage.
    pub toolchains: Vec<ToolchainChoice>,
    /// Cage name (for messages).
    pub name: String,
    /// Source path (for messages).
    pub source: PathBuf,
}

impl Cage {
    /// Compile this cage into a plain field overlay.
    pub fn overlay(&self) -> CageOverlay {
        let mut add_dirs_rw = Vec::new();
        let mut add_dirs_ro = Vec::new();
        for d in &self.dirs {
            match d.access {
                Access::Rw => add_dirs_rw.push(d.path.clone()),
                Access::Ro => add_dirs_ro.push(d.path.clone()),
                other => {
                    // Validated at parse time; keep defensive.
                    let _ = other;
                }
            }
        }
        let (home, ephemeral_home) = match &self.home {
            HomeMode::Inherit => (None, false),
            HomeMode::Ephemeral => (None, true),
            HomeMode::Path(p) => (Some(p.clone()), false),
        };
        CageOverlay {
            profiles: self.profiles.clone(),
            home,
            ephemeral_home,
            add_dirs_rw,
            add_dirs_ro,
            toolchains: self.toolchains.clone(),
            name: self.name.clone(),
            source: self.source.clone(),
        }
    }
}

// --- TOML wire format ---

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CageFile {
    #[serde(default = "default_schema")]
    schema: u32,
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_home_str")]
    home: String,
    #[serde(default)]
    profiles: Vec<String>,
    #[serde(default)]
    dirs: Vec<CageDirFile>,
    /// `[toolchains.<id>] strategy = "link|share|isolate"`.
    #[serde(default)]
    toolchains: HashMap<String, ToolchainFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolchainFile {
    strategy: String,
}

fn default_schema() -> u32 {
    CAGE_SCHEMA
}

fn default_home_str() -> String {
    "inherit".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CageDirFile {
    path: String,
    access: Access,
}

/// Load and validate a cage TOML file.
pub fn load_from_path(path: &Path) -> Result<Cage> {
    let body =
        std::fs::read_to_string(path).ctx(|| format!("reading cage '{}'", path.display()))?;
    let file: CageFile = toml::from_str(&body)
        .map_err(|e| Error::Message(format!("parsing cage '{}': {e}", path.display())))?;

    if file.schema != CAGE_SCHEMA {
        return Err(Error::Message(format!(
            "cage '{}': unsupported schema {} (expected {CAGE_SCHEMA})",
            path.display(),
            file.schema
        )));
    }

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string();
    let name = file.name.unwrap_or(stem);

    let home = HomeMode::parse(&file.home)?;

    let mut dirs = Vec::with_capacity(file.dirs.len());
    for d in file.dirs {
        match d.access {
            Access::Ro | Access::Rw => dirs.push(CageDir {
                path: d.path,
                access: d.access,
            }),
            Access::None | Access::Metadata => {
                return Err(Error::Message(format!(
                    "cage '{}': [[dirs]] access for '{}' must be \"ro\" or \"rw\" (got {:?})",
                    path.display(),
                    d.path,
                    d.access
                )));
            }
        }
    }

    let mut toolchains = Vec::new();
    for (key, tc) in file.toolchains {
        toolchains.push(ToolchainChoice::new(&key, &tc.strategy).map_err(|e| {
            Error::Message(format!(
                "cage '{}': [toolchains.{}]: {e}",
                path.display(),
                key
            ))
        })?);
    }
    // Stable order for reproducible dry-run / tests.
    toolchains.sort_by(|a, b| a.id.cmp(&b.id));

    let source = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    Ok(Cage {
        schema: file.schema,
        name,
        home,
        profiles: file.profiles,
        dirs,
        toolchains,
        source,
    })
}

/// User-level cages directory: `$XDG_CONFIG_HOME/isol8/cages` (or platform equivalent).
pub fn user_cages_dir() -> Option<PathBuf> {
    config_home().map(|h| h.join("isol8").join("cages"))
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
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
}

/// Walk from `start` up to the filesystem root (or git root), yielding each directory.
fn walk_ancestors(start: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut cur = start.to_path_buf();
    loop {
        out.push(cur.clone());
        if cur.join(".git").exists() {
            // Still include this dir; stop climbing further for project-local discovery.
            break;
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p.to_path_buf(),
            _ => break,
        }
    }
    out
}

/// Resolve a cage by optional name using the discovery order in evo-repo §3.2
/// (without CLI/env — those are applied by the caller when choosing `name`).
///
/// When `name` is `Some("work")`, looks for project then user files named `work`.
/// When `name` is `None`, looks for project default (`.isol8/cage.toml`) then
/// `~/.config/isol8/cages/default.toml`.
pub fn resolve(name: Option<&str>, cwd: &Path) -> Result<Option<Cage>> {
    if let Some(n) = name {
        if n.is_empty() {
            return Ok(None);
        }
        // Absolute or relative path to a cage file.
        let as_path = PathBuf::from(n);
        if n.ends_with(".toml") || as_path.is_file() {
            if as_path.is_file() {
                return load_from_path(&as_path).map(Some);
            }
            return Err(Error::Message(format!(
                "cage file not found: '{}'",
                as_path.display()
            )));
        }
        for dir in walk_ancestors(cwd) {
            let candidates = [
                dir.join(".isol8").join("cages").join(format!("{n}.toml")),
                dir.join(".isol8").join(format!("{n}.toml")),
            ];
            for c in candidates {
                if c.is_file() {
                    return load_from_path(&c).map(Some);
                }
            }
        }
        if let Some(user) = user_cages_dir() {
            let c = user.join(format!("{n}.toml"));
            if c.is_file() {
                return load_from_path(&c).map(Some);
            }
        }
        return Err(Error::Message(format!(
            "cage '{n}' not found (looked under .isol8/cages/, .isol8/, and {})",
            user_cages_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "~/.config/isol8/cages".into())
        )));
    }

    // Default cage (no name).
    for dir in walk_ancestors(cwd) {
        let c = dir.join(".isol8").join("cage.toml");
        if c.is_file() {
            return load_from_path(&c).map(Some);
        }
    }
    if let Some(user) = user_cages_dir() {
        let c = user.join("default.toml");
        if c.is_file() {
            return load_from_path(&c).map(Some);
        }
    }
    Ok(None)
}

/// List known cages: `(name, path)` from the user cages dir and project `.isol8/cages/`.
pub fn list_cages(cwd: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push_dir = |dir: &Path| {
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() || !seen.insert(name.clone()) {
                continue;
            }
            out.push((name, p));
        }
    };

    for dir in walk_ancestors(cwd) {
        push_dir(&dir.join(".isol8").join("cages"));
    }
    if let Some(user) = user_cages_dir() {
        push_dir(&user);
    }

    // Project default cage.toml (name from file contents or "default")
    for dir in walk_ancestors(cwd) {
        let c = dir.join(".isol8").join("cage.toml");
        if c.is_file() {
            let name = load_from_path(&c)
                .map(|cg| cg.name)
                .unwrap_or_else(|_| "default".into());
            if seen.insert(name.clone()) {
                out.push((name, c));
            }
            break;
        }
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Template body for `@cage new`.
pub fn new_template(name: &str, home: &str) -> String {
    format!(
        r#"# isol8 cage — named isolation unit (selection layer → Spec)
# See _docs/wip/multi-evo-plan.md Phase 1 / _docs/inbox/evo-repo.md §3
schema = 1
name = "{name}"
home = "{home}"          # inherit | ephemeral | @managed/<id> | /path/to/home

# Profile layers (deny-first merge). Empty list → config default_profiles still apply.
# Non-empty replaces default_profiles (include system-runtime yourself if needed):
# profiles = ["base", "macos/system-runtime", "toolchains/rust"]
profiles = []

# Extra path grants for this cage:
# [[dirs]]
# path = "~/work/acme"
# access = "rw"

# Toolchain recipes (see recipes/ and _docs/recipes.md):
# [toolchains.nvm]
# strategy = "link"
# [toolchains.cargo]
# strategy = "link"
"#
    )
}

/// Write a new cage file under the user cages dir (or `dir` if given).
pub fn write_new(name: &str, home: &str, dir: Option<&Path>) -> Result<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(Error::Message(
            "cage name must be a non-empty bare identifier (no path separators)".into(),
        ));
    }
    HomeMode::parse(home)?; // validate early
    let base = match dir {
        Some(d) => d.to_path_buf(),
        None => user_cages_dir().ok_or_else(|| {
            Error::Message(
                "cannot determine config home for cages (set HOME or XDG_CONFIG_HOME)".into(),
            )
        })?,
    };
    std::fs::create_dir_all(&base)
        .ctx(|| format!("creating cages directory '{}'", base.display()))?;
    let path = base.join(format!("{name}.toml"));
    if path.exists() {
        return Err(Error::Message(format!(
            "cage already exists at {} (refusing to overwrite)",
            path.display()
        )));
    }
    std::fs::write(&path, new_template(name, home))
        .ctx(|| format!("writing cage '{}'", path.display()))?;
    Ok(path)
}

/// Format a cage for `@cage show`.
pub fn format_show(cage: &Cage) -> String {
    let home = match &cage.home {
        HomeMode::Inherit => "inherit".to_string(),
        HomeMode::Ephemeral => "ephemeral".to_string(),
        HomeMode::Path(p) => p.clone(),
    };
    let mut out = String::new();
    out.push_str(&format!("# source: {}\n", cage.source.display()));
    out.push_str(&format!("schema = {}\n", cage.schema));
    out.push_str(&format!("name = {:?}\n", cage.name));
    out.push_str(&format!("home = {home:?}\n"));
    out.push_str(&format!("profiles = {:?}\n", cage.profiles));
    if cage.dirs.is_empty() {
        out.push_str("# dirs: (none)\n");
    } else {
        for d in &cage.dirs {
            let access = format!("{:?}", d.access).to_lowercase();
            out.push_str(&format!(
                "[[dirs]]\npath = {:?}\naccess = \"{access}\"\n",
                d.path
            ));
        }
    }
    if !cage.toolchains.is_empty() {
        out.push('\n');
        for tc in &cage.toolchains {
            let short = tc.id.strip_prefix("toolchains/").unwrap_or(tc.id.as_str());
            out.push_str(&format!(
                "[toolchains.{short}]\nstrategy = {:?}\n",
                tc.strategy.as_str()
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tmp_dir() -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "isol8-cage-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_minimal_cage() {
        let dir = tmp_dir();
        let path = dir.join("work.toml");
        std::fs::write(
            &path,
            r#"
schema = 1
name = "work"
home = "inherit"
profiles = ["base", "toolchains/rust"]
[[dirs]]
path = "~/proj"
access = "rw"
"#,
        )
        .unwrap();
        let c = load_from_path(&path).unwrap();
        assert_eq!(c.name, "work");
        assert_eq!(c.home, HomeMode::Inherit);
        assert_eq!(c.profiles, vec!["base", "toolchains/rust"]);
        assert_eq!(c.dirs.len(), 1);
        assert_eq!(c.dirs[0].access, Access::Rw);
        let o = c.overlay();
        assert!(o.home.is_none());
        assert!(!o.ephemeral_home);
        assert_eq!(o.add_dirs_rw, vec!["~/proj"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_ephemeral_and_path_home() {
        let dir = tmp_dir();
        let p1 = dir.join("e.toml");
        std::fs::write(
            &p1,
            r#"
schema = 1
name = "e"
home = "ephemeral"
profiles = []
"#,
        )
        .unwrap();
        let c = load_from_path(&p1).unwrap();
        assert_eq!(c.home, HomeMode::Ephemeral);
        assert!(c.overlay().ephemeral_home);

        let p2 = dir.join("p.toml");
        std::fs::write(
            &p2,
            r#"
schema = 1
home = "/tmp/my-home"
profiles = ["base"]
"#,
        )
        .unwrap();
        let c = load_from_path(&p2).unwrap();
        assert_eq!(c.home, HomeMode::Path("/tmp/my-home".into()));
        assert_eq!(c.name, "p"); // from stem
        assert_eq!(c.overlay().home.as_deref(), Some("/tmp/my-home"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn managed_home_accepted() {
        let dir = tmp_dir();
        let path = dir.join("m.toml");
        std::fs::write(
            &path,
            r#"
schema = 1
name = "m"
home = "@managed/work"
"#,
        )
        .unwrap();
        let c = load_from_path(&path).unwrap();
        assert_eq!(c.home, HomeMode::Path("@managed/work".into()));
        assert_eq!(c.overlay().home.as_deref(), Some("@managed/work"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_dir_access_rejected() {
        let dir = tmp_dir();
        let path = dir.join("bad.toml");
        std::fs::write(
            &path,
            r#"
schema = 1
name = "bad"
[[dirs]]
path = "/x"
access = "metadata"
"#,
        )
        .unwrap();
        let err = load_from_path(&path).unwrap_err().to_string();
        assert!(err.contains("ro") || err.contains("rw"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toolchains_table_parsed() {
        let dir = tmp_dir();
        let path = dir.join("t.toml");
        std::fs::write(
            &path,
            r#"
schema = 1
name = "t"
home = "inherit"
[toolchains.nvm]
strategy = "link"
[toolchains.cargo]
strategy = "share"
"#,
        )
        .unwrap();
        let c = load_from_path(&path).unwrap();
        assert_eq!(c.toolchains.len(), 2);
        assert_eq!(c.toolchains[0].id, "toolchains/cargo"); // sorted
        assert_eq!(c.toolchains[0].strategy, crate::recipe::StrategyName::Share);
        assert_eq!(c.toolchains[1].id, "toolchains/nvm");
        assert_eq!(c.toolchains[1].strategy, crate::recipe::StrategyName::Link);
        let o = c.overlay();
        assert_eq!(o.toolchains.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_named_from_project() {
        let root = tmp_dir();
        let cages = root.join(".isol8").join("cages");
        std::fs::create_dir_all(&cages).unwrap();
        std::fs::write(
            cages.join("work.toml"),
            r#"
schema = 1
name = "work"
home = "inherit"
profiles = ["base"]
"#,
        )
        .unwrap();
        let c = resolve(Some("work"), &root).unwrap().unwrap();
        assert_eq!(c.name, "work");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_default_project_cage() {
        let root = tmp_dir();
        let isol8 = root.join(".isol8");
        std::fs::create_dir_all(&isol8).unwrap();
        std::fs::write(
            isol8.join("cage.toml"),
            r#"
schema = 1
name = "project"
home = "ephemeral"
profiles = ["base"]
"#,
        )
        .unwrap();
        let c = resolve(None, &root).unwrap().unwrap();
        assert_eq!(c.name, "project");
        assert_eq!(c.home, HomeMode::Ephemeral);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_missing_named_errors() {
        let root = tmp_dir();
        let err = resolve(Some("nope"), &root).unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_new_and_list() {
        let dir = tmp_dir();
        let path = write_new("demo", "inherit", Some(&dir)).unwrap();
        assert!(path.is_file());
        let c = load_from_path(&path).unwrap();
        assert_eq!(c.name, "demo");

        // Second write refuses overwrite.
        assert!(write_new("demo", "inherit", Some(&dir)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_schema_rejected() {
        let dir = tmp_dir();
        let path = dir.join("x.toml");
        std::fs::write(&path, "schema = 99\nname = \"x\"\n").unwrap();
        assert!(load_from_path(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
