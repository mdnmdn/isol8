//! Home materialization plan/apply.
//!
//! Every mutating home operation is computed first ([`HomePlan::compute`]) with no
//! side effects, then applied ([`HomePlan::apply`]). Dry-run, wizard preview, and
//! spawn share this path. See evo-repo §4.2 / §7.2 and multi-evo-plan Phase 2.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::context::Context;
use crate::error::{Error, Result, ResultExt};
use crate::home::{expand_tilde, REAL_HOME_TOKEN};

/// Kind of filesystem materialization under a (possibly replaced) home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HomeOpKind {
    /// Symlink `to` → `from` (typically replaced-home path → real-home path).
    Link,
    /// Create a directory (and parents).
    Mkdir,
    /// Copy `from` → `to` read-only; first-creation-only (skip if `to` exists).
    SeedRo,
    /// Copy `from` → `to` (writable); skip if `to` already exists (idempotent).
    Copy,
}

impl HomeOpKind {
    /// Lowercase label for rendering.
    pub fn as_str(self) -> &'static str {
        match self {
            HomeOpKind::Link => "link",
            HomeOpKind::Mkdir => "mkdir",
            HomeOpKind::SeedRo => "seed-ro",
            HomeOpKind::Copy => "copy",
        }
    }
}

/// Unexpanded materialization op (tokens still present). Built from profile seeds,
/// future recipes, or [`crate::sandbox::Spec::home_ops`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HomeOpSpec {
    /// Operation kind.
    pub kind: HomeOpKind,
    /// Source path tokens (`link`/`seed-ro`/`copy`).
    pub from: Option<String>,
    /// Destination path tokens (`link`/`seed-ro`/`copy`).
    pub to: Option<String>,
    /// Single path tokens (`mkdir`).
    pub path: Option<String>,
}

impl HomeOpSpec {
    /// Symlink `to` → `from`.
    pub fn link(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            kind: HomeOpKind::Link,
            from: Some(from.into()),
            to: Some(to.into()),
            path: None,
        }
    }

    /// Create directory at `path`.
    pub fn mkdir(path: impl Into<String>) -> Self {
        Self {
            kind: HomeOpKind::Mkdir,
            from: None,
            to: None,
            path: Some(path.into()),
        }
    }

    /// Read-only seed copy `from` → `to` (first-creation-only).
    pub fn seed_ro(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            kind: HomeOpKind::SeedRo,
            from: Some(from.into()),
            to: Some(to.into()),
            path: None,
        }
    }

    /// Writable copy `from` → `to` (skip if destination exists).
    pub fn copy(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            kind: HomeOpKind::Copy,
            from: Some(from.into()),
            to: Some(to.into()),
            path: None,
        }
    }
}

/// What apply would do for one expanded op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PlanAction {
    /// Perform the operation on apply.
    #[serde(rename = "apply")]
    Apply,
    /// Destination already in the desired state.
    #[serde(rename = "skip-exists")]
    SkipExists,
    /// Source missing — best-effort skip (seed/copy/link).
    #[serde(rename = "skip-missing")]
    SkipMissingSource,
}

/// One expanded, classified materialization step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedOp {
    /// Operation kind.
    pub kind: HomeOpKind,
    /// Absolute source (link/seed-ro/copy).
    pub from: Option<PathBuf>,
    /// Absolute destination (link/seed-ro/copy).
    pub to: Option<PathBuf>,
    /// Absolute path (mkdir).
    pub path: Option<PathBuf>,
    /// Classification for apply / dry-run display.
    pub action: PlanAction,
}

/// Idempotent home materialization plan (no side effects until [`HomePlan::apply`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HomePlan {
    /// Ordered steps.
    pub ops: Vec<PlannedOp>,
}

impl HomePlan {
    /// Expand token specs against `ctx` + effective home and classify actions.
    ///
    /// No filesystem mutations (reads only for classification).
    pub fn compute(specs: &[HomeOpSpec], ctx: &Context, effective_home: &Path) -> Result<HomePlan> {
        let mut ops = Vec::with_capacity(specs.len());
        for spec in specs {
            ops.push(plan_one(spec, ctx, effective_home)?);
        }
        Ok(HomePlan { ops })
    }

    /// Apply every op with `action == Apply`. Idempotent when re-run after success.
    pub fn apply(&self) -> Result<()> {
        for op in &self.ops {
            if op.action != PlanAction::Apply {
                continue;
            }
            apply_one(op)?;
        }
        Ok(())
    }

    /// Human-readable plan for `--show-policies` / dry-run.
    pub fn render(&self) -> String {
        if self.ops.is_empty() {
            return "(none)\n".into();
        }
        let mut out = String::new();
        for op in &self.ops {
            let tag = match op.action {
                PlanAction::Apply => "apply",
                PlanAction::SkipExists => "skip-exists",
                PlanAction::SkipMissingSource => "skip-missing",
            };
            match op.kind {
                HomeOpKind::Mkdir => {
                    let p = op
                        .path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    out.push_str(&format!("  [{tag}] mkdir {p}\n"));
                }
                HomeOpKind::Link => {
                    let from = op
                        .from
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    let to = op
                        .to
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    out.push_str(&format!("  [{tag}] link {to} -> {from}\n"));
                }
                HomeOpKind::SeedRo | HomeOpKind::Copy => {
                    let from = op
                        .from
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    let to = op
                        .to
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    out.push_str(&format!("  [{tag}] {} {from} -> {to}\n", op.kind.as_str()));
                }
            }
        }
        out
    }

    /// Number of ops that will mutate the filesystem.
    pub fn apply_count(&self) -> usize {
        self.ops
            .iter()
            .filter(|o| o.action == PlanAction::Apply)
            .count()
    }
}

/// Expand a path token string: `#HOME` → real home, `~` → effective home,
/// `@managed/<id>` → `{config_dir}/homes/<id>`, other `@…` → config-relative.
/// Absolute paths pass through.
pub fn expand_tokens(raw: &str, ctx: &Context, effective_home: &Path) -> Result<PathBuf> {
    let s = if let Some(id) = raw.strip_prefix("@managed/") {
        return ctx.managed_home(id);
    } else if let Some(p) = ctx.expand_at(raw) {
        return Ok(p);
    } else if raw.contains(REAL_HOME_TOKEN) {
        raw.replace(REAL_HOME_TOKEN, &ctx.real_home.to_string_lossy())
    } else {
        raw.to_string()
    };
    let expanded = expand_tilde(&s, effective_home);
    #[cfg(windows)]
    {
        Ok(PathBuf::from(crate::home::expand_windows_vars(&expanded)))
    }
    #[cfg(not(windows))]
    {
        Ok(PathBuf::from(expanded))
    }
}

fn plan_one(spec: &HomeOpSpec, ctx: &Context, effective_home: &Path) -> Result<PlannedOp> {
    match spec.kind {
        HomeOpKind::Mkdir => {
            let path = spec
                .path
                .as_deref()
                .ok_or_else(|| Error::Message("home op mkdir requires `path`".into()))?;
            let path = expand_tokens(path, ctx, effective_home)?;
            let action = if path.is_dir() {
                PlanAction::SkipExists
            } else if path.exists() {
                return Err(Error::Message(format!(
                    "mkdir target exists and is not a directory: {}",
                    path.display()
                )));
            } else {
                PlanAction::Apply
            };
            Ok(PlannedOp {
                kind: HomeOpKind::Mkdir,
                from: None,
                to: None,
                path: Some(path),
                action,
            })
        }
        HomeOpKind::Link => {
            let from = spec
                .from
                .as_deref()
                .ok_or_else(|| Error::Message("home op link requires `from`".into()))?;
            let to = spec
                .to
                .as_deref()
                .ok_or_else(|| Error::Message("home op link requires `to`".into()))?;
            let from = expand_tokens(from, ctx, effective_home)?;
            let to = expand_tokens(to, ctx, effective_home)?;
            let action = classify_link(&from, &to);
            Ok(PlannedOp {
                kind: HomeOpKind::Link,
                from: Some(from),
                to: Some(to),
                path: None,
                action,
            })
        }
        HomeOpKind::SeedRo | HomeOpKind::Copy => {
            let from = spec.from.as_deref().ok_or_else(|| {
                Error::Message(format!("home op {} requires `from`", spec.kind.as_str()))
            })?;
            let to = spec.to.as_deref().ok_or_else(|| {
                Error::Message(format!("home op {} requires `to`", spec.kind.as_str()))
            })?;
            let from = expand_tokens(from, ctx, effective_home)?;
            let to = expand_tokens(to, ctx, effective_home)?;
            let action = if !from.exists() {
                PlanAction::SkipMissingSource
            } else if to.exists() {
                PlanAction::SkipExists
            } else {
                PlanAction::Apply
            };
            Ok(PlannedOp {
                kind: spec.kind,
                from: Some(from),
                to: Some(to),
                path: None,
                action,
            })
        }
    }
}

fn classify_link(from: &Path, to: &Path) -> PlanAction {
    if !from.exists() {
        return PlanAction::SkipMissingSource;
    }
    match std::fs::symlink_metadata(to) {
        Ok(meta) if meta.file_type().is_symlink() => {
            // Already a symlink — treat as done (idempotent). Don't require
            // same target string equality across path normalizations.
            PlanAction::SkipExists
        }
        Ok(_) => {
            // Exists but not a symlink — apply will error with a clear message.
            PlanAction::Apply
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PlanAction::Apply,
        Err(_) => PlanAction::Apply,
    }
}

fn apply_one(op: &PlannedOp) -> Result<()> {
    match op.kind {
        HomeOpKind::Mkdir => {
            let path = op.path.as_ref().expect("mkdir path");
            std::fs::create_dir_all(path).ctx(|| format!("mkdir {}", path.display()))?;
            Ok(())
        }
        HomeOpKind::Link => {
            let from = op.from.as_ref().expect("link from");
            let to = op.to.as_ref().expect("link to");
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .ctx(|| format!("creating parent {}", parent.display()))?;
            }
            if to.exists() || std::fs::symlink_metadata(to).is_ok() {
                let meta = std::fs::symlink_metadata(to)
                    .ctx(|| format!("stat link destination {}", to.display()))?;
                if meta.file_type().is_symlink() {
                    return Ok(()); // race: became correct between plan and apply
                }
                return Err(Error::Message(format!(
                    "cannot create symlink at {}: path exists and is not a symlink",
                    to.display()
                )));
            }
            symlink_path(from, to)
                .ctx(|| format!("symlink {} -> {}", to.display(), from.display()))?;
            Ok(())
        }
        HomeOpKind::SeedRo => {
            let from = op.from.as_ref().expect("seed from");
            let to = op.to.as_ref().expect("seed to");
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).ctx(|| format!("creating {}", parent.display()))?;
            }
            copy_readonly(from, to)
                .ctx(|| format!("seed-ro {} -> {}", from.display(), to.display()))?;
            Ok(())
        }
        HomeOpKind::Copy => {
            let from = op.from.as_ref().expect("copy from");
            let to = op.to.as_ref().expect("copy to");
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).ctx(|| format!("creating {}", parent.display()))?;
            }
            copy_writable(from, to)
                .ctx(|| format!("copy {} -> {}", from.display(), to.display()))?;
            Ok(())
        }
    }
}

fn symlink_path(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(from, to)
    }
    #[cfg(windows)]
    {
        // Directory vs file: prefer dir symlink when source is a directory.
        if from.is_dir() {
            std::os::windows::fs::symlink_dir(from, to)
        } else {
            std::os::windows::fs::symlink_file(from, to)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (from, to);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symlinks not supported on this platform",
        ))
    }
}

/// Recursively copy `src` → `dst`, marking files read-only. Skip if `dst` exists
/// (first-creation-only).
fn copy_readonly(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_readonly(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if dst.exists() {
            return Ok(());
        }
        std::fs::copy(src, dst)?;
        let mut perms = std::fs::metadata(dst)?.permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(dst, perms)?;
    }
    Ok(())
}

/// Recursively copy `src` → `dst` without forcing read-only. Skip if `dst` exists.
fn copy_writable(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_writable(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if dst.exists() {
            return Ok(());
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// Build seed-ro specs from profile seed entries (`~/…` relative to real home).
pub fn seed_specs_from_list(seed: &[String]) -> Vec<HomeOpSpec> {
    seed.iter()
        .map(|entry| {
            // Seeds are real-home-relative; write into the effective home at the
            // same relative path.
            let rel = entry.strip_prefix("~/").unwrap_or(entry.as_str());
            HomeOpSpec::seed_ro(format!("#HOME/{rel}"), format!("~/{rel}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Context, Platform};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tmp() -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "isol8-plan-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx(real: &Path, managed: &Path) -> Context {
        // config_dir is parent of managed when managed ends in "homes"; else sibling.
        let config_dir = if managed.ends_with("homes") {
            managed
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| managed.to_path_buf())
        } else {
            managed.to_path_buf()
        };
        Context {
            real_home: real.to_path_buf(),
            cwd: PathBuf::from("/tmp"),
            platform: Platform::Linux,
            config_dir,
            managed_root: managed.to_path_buf(),
        }
    }

    #[test]
    fn expand_tokens_home_and_managed() {
        let c = ctx(Path::new("/real"), Path::new("/data/homes"));
        let eff = Path::new("/eff");
        assert_eq!(
            expand_tokens("#HOME/.nvm", &c, eff).unwrap(),
            PathBuf::from("/real/.nvm")
        );
        assert_eq!(
            expand_tokens("~/.nvm", &c, eff).unwrap(),
            PathBuf::from("/eff/.nvm")
        );
        assert_eq!(
            expand_tokens("@managed/work", &c, eff).unwrap(),
            PathBuf::from("/data/homes/work")
        );
        assert_eq!(
            expand_tokens("@/profiles", &c, eff).unwrap(),
            PathBuf::from("/data/profiles")
        );
    }

    #[test]
    fn plan_apply_mkdir_idempotent() {
        let root = tmp();
        let real = root.join("real");
        let eff = root.join("eff");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&eff).unwrap();
        let c = ctx(&real, &root.join("managed"));

        let specs = vec![HomeOpSpec::mkdir("~/.cache/foo")];
        let plan = HomePlan::compute(&specs, &c, &eff).unwrap();
        assert_eq!(plan.apply_count(), 1);
        plan.apply().unwrap();
        assert!(eff.join(".cache/foo").is_dir());

        let plan2 = HomePlan::compute(&specs, &c, &eff).unwrap();
        assert_eq!(plan2.apply_count(), 0);
        plan2.apply().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_apply_seed_ro_and_link() {
        let root = tmp();
        let real = root.join("real");
        let eff = root.join("eff");
        std::fs::create_dir_all(real.join(".nvm")).unwrap();
        std::fs::write(real.join(".nvm/x"), b"v").unwrap();
        std::fs::write(real.join(".gitconfig"), b"cfg").unwrap();
        std::fs::create_dir_all(&eff).unwrap();
        let c = ctx(&real, &root.join("managed"));

        let specs = vec![
            HomeOpSpec::seed_ro("#HOME/.gitconfig", "~/.gitconfig"),
            HomeOpSpec::link("#HOME/.nvm", "~/.nvm"),
        ];
        let plan = HomePlan::compute(&specs, &c, &eff).unwrap();
        assert_eq!(plan.apply_count(), 2);
        plan.apply().unwrap();

        assert_eq!(std::fs::read(eff.join(".gitconfig")).unwrap(), b"cfg");
        assert!(std::fs::symlink_metadata(eff.join(".nvm"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(eff.join(".nvm/x")).unwrap(), b"v");

        // Idempotent re-apply
        let plan2 = HomePlan::compute(&specs, &c, &eff).unwrap();
        assert_eq!(plan2.apply_count(), 0);
        plan2.apply().unwrap();
        // seed-ro keeps first snapshot
        std::fs::write(real.join(".gitconfig"), b"new").unwrap();
        let plan3 = HomePlan::compute(&specs, &c, &eff).unwrap();
        plan3.apply().unwrap();
        assert_eq!(std::fs::read(eff.join(".gitconfig")).unwrap(), b"cfg");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_specs_from_list_shape() {
        let specs = seed_specs_from_list(&["~/.gitconfig".into()]);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].kind, HomeOpKind::SeedRo);
        assert_eq!(specs[0].from.as_deref(), Some("#HOME/.gitconfig"));
        assert_eq!(specs[0].to.as_deref(), Some("~/.gitconfig"));
    }

    #[test]
    fn missing_source_skips() {
        let root = tmp();
        let real = root.join("real");
        let eff = root.join("eff");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&eff).unwrap();
        let c = ctx(&real, &root.join("managed"));
        let plan = HomePlan::compute(
            &[HomeOpSpec::link("#HOME/.missing", "~/.missing")],
            &c,
            &eff,
        )
        .unwrap();
        assert_eq!(plan.ops[0].action, PlanAction::SkipMissingSource);
        plan.apply().unwrap();
        assert!(!eff.join(".missing").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
