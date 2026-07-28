//! Policy diagnosis: denials → recipe suggestions (`--analyze`).
//!
//! Shared post-processing is platform-independent. Backends (or a test feed)
//! supply NDJSON denials; this module collapses them, matches recipe path
//! prefixes, and classifies missing home materialization.
//!
//! Phase 5: shared layer + NDJSON I/O. Phase 6: macOS unified-log scrape
//! ([`crate::analyze_macos`]). Windows hook still deferred (R2 documentary).
//! Linux shadow mode is Phase 10.
//!
//! See evo-repo §8.4 and `_docs/wip/multi-evo-plan.md` Phases 5–6.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::error::{Error, Result, ResultExt};
use crate::filter::RunContext;
use crate::home::{expand_tilde, REAL_HOME_TOKEN};
use crate::recipe::{RecipeRegistry, StrategyName};
use crate::sandbox::Spec;

/// Kind of denied access (normalized across backends).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DenialAccess {
    /// File/directory read.
    Read,
    /// File/directory write.
    Write,
    /// Execute.
    Exec,
    /// Metadata / stat only.
    Metadata,
    /// Unclassified.
    Other,
}

impl DenialAccess {
    /// Short label for reports (`r`, `w`, `x`, `m`, `?`).
    pub fn short(self) -> &'static str {
        match self {
            DenialAccess::Read => "r",
            DenialAccess::Write => "w",
            DenialAccess::Exec => "x",
            DenialAccess::Metadata => "m",
            DenialAccess::Other => "?",
        }
    }

    /// Parse NDJSON / human labels.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "r" | "read" | "ro" | "file-read" | "file-read-data" | "file-read-metadata"
            | "open-read" => DenialAccess::Read,
            "w" | "write" | "rw" | "file-write" | "file-write-data" | "open-write" => {
                DenialAccess::Write
            }
            "x" | "exec" | "execute" | "file-read-xattr" => DenialAccess::Exec,
            "m" | "metadata" | "stat" => DenialAccess::Metadata,
            "rx" | "read-exec" => DenialAccess::Read, // treat combo as read for matching
            _ => DenialAccess::Other,
        }
    }
}

/// One observed access denial (or a synthetic feed record).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Denial {
    /// Absolute path that was denied (as observed).
    pub path: PathBuf,
    /// Access kind.
    pub access: DenialAccess,
    /// How many times this exact path+access was seen (before collapse).
    #[serde(default = "one")]
    pub count: u32,
    /// Process id that hit the denial (0 if unknown).
    #[serde(default)]
    pub pid: u32,
    /// Executable / module name when known (Windows hook can fill this).
    #[serde(default)]
    pub exe: Option<String>,
}

fn one() -> u32 {
    1
}

/// Wire format for one NDJSON line (flexible access field).
#[derive(Debug, Deserialize)]
struct DenialLine {
    path: String,
    #[serde(default)]
    access: Option<String>,
    #[serde(default)]
    count: Option<u32>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    exe: Option<String>,
}

/// A path prefix published by a recipe strategy (for matching denials).
#[derive(Debug, Clone, Serialize)]
pub struct RecipePathIndex {
    /// Recipe id (`toolchains/nvm`).
    pub id: String,
    /// Strategy that contributes this prefix.
    pub strategy: StrategyName,
    /// Expanded absolute prefix (real-home or effective-home based).
    pub prefix: PathBuf,
    /// True when the grant token used `#HOME` / real-home (vs `~` only).
    pub real_home_grant: bool,
    /// True when the strategy includes a `link` home op for this area.
    pub has_link_op: bool,
}

/// One collapsed denial root with a suggestion.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisItem {
    /// Collapsed path root.
    pub root: PathBuf,
    /// Dominant access kind for this root.
    pub access: DenialAccess,
    /// Total denial count under this root.
    pub count: u32,
    /// Matching recipe, if any.
    pub recipe_id: Option<String>,
    /// Suggested strategy for that recipe.
    pub strategy: Option<StrategyName>,
    /// True when a home materialization link is likely needed.
    pub needs_home_link: bool,
    /// Extra human note.
    pub note: String,
}

/// Full analysis report.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisReport {
    /// Sum of raw denial counts before collapse.
    pub total_denials: u32,
    /// Number of collapsed roots.
    pub root_count: usize,
    /// Where denials came from (for the caveat line).
    pub source_note: String,
    /// Collapsed items, highest count first.
    pub items: Vec<AnalysisItem>,
}

impl AnalysisReport {
    /// Human-readable report for the CLI.
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "Observed {} denials ({} roots)\n",
            self.total_denials, self.root_count
        ));
        s.push_str(&format!("Source: {}\n", self.source_note));
        s.push_str(
            "Note: report lists *observed* denials only — not an exhaustive audit \
             (user-mode hooks / log scrapes can miss paths).\n\n",
        );
        if self.items.is_empty() {
            s.push_str("  (no denials to analyze)\n");
            return s;
        }
        for it in &self.items {
            let recipe = match (&it.recipe_id, &it.strategy) {
                (Some(id), Some(st)) => format!("{id}  strategy={}", st.as_str()),
                (Some(id), None) => id.clone(),
                _ => "no match; add a grant or recipe manually?".into(),
            };
            let link = if it.needs_home_link {
                "  [needs home link]"
            } else {
                ""
            };
            s.push_str(&format!(
                "  {:<28} {:>5} {:<2}  → {recipe}{link}\n",
                it.root.display(),
                it.count,
                it.access.short(),
            ));
            if !it.note.is_empty() {
                s.push_str(&format!("      {}\n", it.note));
            }
        }
        // Suggested cage fix line
        let mut ids: Vec<String> = self
            .items
            .iter()
            .filter_map(|i| i.recipe_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        if !ids.is_empty() {
            s.push('\n');
            for id in &ids {
                let bare = id.strip_prefix("toolchains/").unwrap_or(id);
                s.push_str(&format!(
                    "  tip: cage  [toolchains.{bare}] strategy = \"link\"  # or share/isolate\n"
                ));
            }
            s.push_str("  tip: isol8 @cage verify <name>  # smoke-test after adding toolchains\n");
        }
        s
    }
}

/// Parse NDJSON denials from a string (one JSON object per line).
pub fn parse_ndjson(body: &str) -> Result<Vec<Denial>> {
    let mut out = Vec::new();
    for (lineno, line) in body.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let row: DenialLine = serde_json::from_str(line).map_err(|e| {
            Error::Message(format!("analyze NDJSON line {}: {e}  ({line})", lineno + 1))
        })?;
        out.push(Denial {
            path: PathBuf::from(row.path),
            access: DenialAccess::parse(row.access.as_deref().unwrap_or("read")),
            count: row.count.unwrap_or(1).max(1),
            pid: row.pid.unwrap_or(0),
            exe: row.exe,
        });
    }
    Ok(out)
}

/// Load denials from an NDJSON file.
pub fn load_ndjson_file(path: &Path) -> Result<Vec<Denial>> {
    let body = std::fs::read_to_string(path)
        .ctx(|| format!("reading analyze feed '{}'", path.display()))?;
    parse_ndjson(&body)
}

/// Default post-run denial log path for a child pid (Windows hook / future).
pub fn default_denial_log_path(pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("isol8-analyze-{pid}.ndjson"))
}

/// Build a recipe path index for the current platform (all strategies).
pub fn build_recipe_index(
    reg: &RecipeRegistry,
    ctx: &RunContext,
    ambient: &Context,
    effective_home: &Path,
) -> Result<Vec<RecipePathIndex>> {
    let mut out = Vec::new();
    for id in reg.ids() {
        let Ok(recipe) = reg.resolve(&id, ctx) else {
            continue;
        };
        for (strat_name, bodies) in &recipe.strategies {
            // Pick matching body (same rules as compile).
            let body = bodies.iter().find(|b| match &b.filter {
                None => true,
                Some(f) => crate::filter::filter_matches(f, ctx),
            });
            let Some(body) = body else {
                continue;
            };
            let has_link = body
                .home
                .iter()
                .any(|op| op.kind == crate::plan::HomeOpKind::Link);
            for grant in &body.paths {
                let real_home_grant =
                    grant.path.contains(REAL_HOME_TOKEN) || grant.path.starts_with("#HOME");
                let expanded = expand_token_path(&grant.path, ambient, effective_home);
                out.push(RecipePathIndex {
                    id: recipe.id.clone(),
                    strategy: *strat_name,
                    prefix: expanded,
                    real_home_grant,
                    has_link_op: has_link,
                });
            }
            // Also index link `from` targets as real-home prefixes.
            for op in &body.home {
                if op.kind == crate::plan::HomeOpKind::Link {
                    if let Some(from) = &op.from {
                        let expanded = expand_token_path(from, ambient, effective_home);
                        out.push(RecipePathIndex {
                            id: recipe.id.clone(),
                            strategy: *strat_name,
                            prefix: expanded,
                            real_home_grant: true,
                            has_link_op: true,
                        });
                    }
                }
            }
        }
    }
    Ok(out)
}

fn expand_token_path(raw: &str, ambient: &Context, effective_home: &Path) -> PathBuf {
    let s = if raw.contains(REAL_HOME_TOKEN) {
        raw.replace(REAL_HOME_TOKEN, &ambient.real_home.to_string_lossy())
    } else {
        raw.to_string()
    };
    // `~` against effective home; for pure real-home tokens there is no `~`.
    PathBuf::from(expand_tilde(&s, effective_home))
}

/// Collapse raw denials to home-child roots (e.g. `~/.m2/…` → `~/.m2`).
pub fn collapse_to_roots(
    denials: &[Denial],
    real_home: &Path,
    effective_home: &Path,
) -> Vec<Denial> {
    // Aggregate exact paths first.
    let mut exact: HashMap<(PathBuf, DenialAccess), Denial> = HashMap::new();
    for d in denials {
        let key = (d.path.clone(), d.access);
        exact
            .entry(key)
            .and_modify(|e| e.count = e.count.saturating_add(d.count))
            .or_insert_with(|| d.clone());
    }

    // Map each path to a collapse root.
    let mut roots: HashMap<(PathBuf, DenialAccess), u32> = HashMap::new();
    let mut meta: HashMap<(PathBuf, DenialAccess), Denial> = HashMap::new();
    for ((path, access), d) in exact {
        let root = collapse_root(&path, real_home, effective_home);
        let key = (root.clone(), access);
        *roots.entry(key.clone()).or_insert(0) += d.count;
        meta.entry(key).or_insert(Denial {
            path: root,
            access,
            count: 0,
            pid: d.pid,
            exe: d.exe.clone(),
        });
    }

    let mut out: Vec<Denial> = roots
        .into_iter()
        .map(|(key, count)| {
            let mut d = meta.remove(&key).unwrap();
            d.count = count;
            d
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.path.cmp(&b.path)));
    out
}

/// Collapse `path` to a stable root: stop at first child of real/effective home,
/// or at the first two path components under root if outside homes.
fn collapse_root(path: &Path, real_home: &Path, effective_home: &Path) -> PathBuf {
    for home in [real_home, effective_home] {
        if let Ok(rel) = path.strip_prefix(home) {
            let mut comps = rel.components();
            if let Some(first) = comps.next() {
                return home.join(first);
            }
            return home.to_path_buf();
        }
    }
    // Outside homes: keep up to depth 3 (e.g. /Users/x/foo or /var/folders/…)
    let comps: Vec<_> = path.components().collect();
    if comps.len() <= 3 {
        return path.to_path_buf();
    }
    PathBuf::from_iter(comps.into_iter().take(4))
}

/// Match a collapsed denial root against the recipe index.
pub fn match_recipe<'a>(root: &Path, index: &'a [RecipePathIndex]) -> Option<&'a RecipePathIndex> {
    let mut best: Option<&RecipePathIndex> = None;
    let mut best_len = 0usize;
    for entry in index {
        if path_under_prefix(root, &entry.prefix) || path_under_prefix(&entry.prefix, root) {
            let len = entry.prefix.as_os_str().len();
            if len >= best_len {
                best_len = len;
                best = Some(entry);
            }
        }
    }
    best
}

fn path_under_prefix(path: &Path, prefix: &Path) -> bool {
    path == prefix || path.starts_with(prefix)
}

/// Classify whether a denial under the effective home likely needs a home link
/// (path exists on real home at the same relative location).
pub fn needs_home_link(
    root: &Path,
    effective_home: &Path,
    real_home: &Path,
    matched: Option<&RecipePathIndex>,
) -> bool {
    if matched.is_some_and(|m| m.has_link_op) {
        // Strategy already wants a link; if denial is under effective home and
        // real counterpart exists, flag it.
        if let Ok(rel) = root.strip_prefix(effective_home) {
            return real_home.join(rel).exists();
        }
        // Denial on real path while running with replaced home often means the
        // agent looked up via #HOME-expanded path without a link at `~`.
        if root.starts_with(real_home) {
            return true;
        }
    }
    // Heuristic without recipe match: denial under effective home, same rel exists on real.
    if let Ok(rel) = root.strip_prefix(effective_home) {
        let real = real_home.join(rel);
        if real.exists() {
            return true;
        }
    }
    false
}

/// Run the full shared analysis pipeline on a denial list.
pub fn analyze(
    denials: &[Denial],
    index: &[RecipePathIndex],
    ambient: &Context,
    effective_home: &Path,
    source_note: impl Into<String>,
) -> AnalysisReport {
    let total: u32 = denials.iter().map(|d| d.count).sum();
    let collapsed = collapse_to_roots(denials, &ambient.real_home, effective_home);
    let mut items = Vec::with_capacity(collapsed.len());
    for d in collapsed {
        let matched = match_recipe(&d.path, index);
        let link = needs_home_link(&d.path, effective_home, &ambient.real_home, matched);
        let note = if link {
            "path exists on real home — prefer a link strategy / home materialization".into()
        } else {
            String::new()
        };
        items.push(AnalysisItem {
            root: d.path,
            access: d.access,
            count: d.count,
            recipe_id: matched.map(|m| m.id.clone()),
            strategy: matched.map(|m| m.strategy),
            needs_home_link: link,
            note,
        });
    }
    let root_count = items.len();
    AnalysisReport {
        total_denials: total,
        root_count,
        source_note: source_note.into(),
        items,
    }
}

/// Resolve the analyze feed path (env override, then optional pid log).
pub fn resolve_feed_path(pid: Option<u32>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ISOL8_ANALYZE_FEED") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    if let Some(pid) = pid {
        let p = default_denial_log_path(pid);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Platform note for --analyze when no denials were collected.
pub fn no_observation_note() -> &'static str {
    match std::env::consts::OS {
        "windows" => {
            "Windows path denials are not recorded yet (AppContainer R2 is documentary; \
             isol8-winhook is not wired). Feed synthetic NDJSON via ISOL8_ANALYZE_FEED \
             to exercise the shared analyzer."
        }
        "macos" => {
            "macOS log stream reported no Sandbox denials for this run \
             (command may not have hit a deny, or log privacy settings blocked the stream). \
             You can still feed NDJSON via ISOL8_ANALYZE_FEED."
        }
        "linux" => {
            "Linux denial observation is deferred (Phase 10 shadow mode). \
             Landlock does not log denials. Feed NDJSON via ISOL8_ANALYZE_FEED."
        }
        _ => "No denial observer on this platform. Use ISOL8_ANALYZE_FEED with NDJSON.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Context, Platform};

    fn ambient(real: &str) -> Context {
        let config_dir = PathBuf::from(format!("{real}/.config/isol8"));
        Context {
            real_home: PathBuf::from(real),
            cwd: PathBuf::from("/tmp"),
            platform: Platform::Macos,
            managed_root: config_dir.join("homes"),
            config_dir,
        }
    }

    #[test]
    fn parse_and_collapse_m2() {
        let ndjson = r#"
{"path":"/Users/u/.m2/repository/a/1","access":"read","count":10}
{"path":"/Users/u/.m2/repository/b/2","access":"r","count":5}
{"path":"/Users/u/.nvm/versions/node/v20/bin/node","access":"read","count":3}
"#;
        let denials = parse_ndjson(ndjson).unwrap();
        assert_eq!(denials.len(), 3);
        let real = Path::new("/Users/u");
        let eff = Path::new("/tmp/eff");
        let roots = collapse_to_roots(&denials, real, eff);
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].path, PathBuf::from("/Users/u/.m2"));
        assert_eq!(roots[0].count, 15);
        assert_eq!(roots[1].path, PathBuf::from("/Users/u/.nvm"));
        assert_eq!(roots[1].count, 3);
    }

    #[test]
    fn match_maven_and_nvm() {
        // Use builtin registry (has nvm/cargo/maven) — no private insert API.
        let reg = RecipeRegistry::load(&[]).unwrap();
        let ctx = RunContext {
            cmd: vec![],
            os: "macos".into(),
            arch: "aarch64".into(),
        };
        let amb = ambient("/Users/u");
        let eff = PathBuf::from("/tmp/eff");
        let index = build_recipe_index(&reg, &ctx, &amb, &eff).unwrap();
        assert!(
            !index.is_empty(),
            "builtin recipes should publish path prefixes"
        );

        let denials = parse_ndjson(
            r#"
{"path":"/Users/u/.m2/repository/x","access":"read","count":100}
{"path":"/Users/u/.nvm/versions/v20","access":"read","count":20}
{"path":"/Users/u/.config/gh/hosts.yml","access":"read","count":2}
"#,
        )
        .unwrap();

        let report = analyze(&denials, &index, &amb, &eff, "fixture");
        assert_eq!(report.total_denials, 122);
        let m2 = report
            .items
            .iter()
            .find(|i| i.root.ends_with(".m2"))
            .unwrap();
        assert_eq!(m2.recipe_id.as_deref(), Some("toolchains/maven"));
        assert!(m2.needs_home_link);
        let nvm = report
            .items
            .iter()
            .find(|i| i.root.ends_with(".nvm"))
            .unwrap();
        assert_eq!(nvm.recipe_id.as_deref(), Some("toolchains/nvm"));
        assert!(
            report.items.iter().any(|i| i.recipe_id.is_none()),
            "gh path should be unmatched: {:?}",
            report.items
        );
        let text = report.render();
        assert!(text.contains("toolchains/maven"));
        assert!(text.contains("Observed"));
    }

    #[test]
    fn needs_home_link_when_real_exists() {
        let tmp = std::env::temp_dir().join(format!("isol8-an-{}", std::process::id()));
        let real = tmp.join("real");
        let eff = tmp.join("eff");
        std::fs::create_dir_all(real.join(".nvm")).unwrap();
        std::fs::create_dir_all(&eff).unwrap();
        let root = eff.join(".nvm");
        assert!(needs_home_link(&root, &eff, &real, None));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

// ---------------------------------------------------------------------------
// End-to-end: run a command and analyze what it was denied
// ---------------------------------------------------------------------------

/// Result of [`run_and_analyze`]: the confined run plus its denial report.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct AnalyzeOutcome {
    /// Exit code of the confined command.
    pub code: i32,
    /// Pid of the confined child (`0` if it never launched).
    pub pid: u32,
    /// Collapsed denials matched against the recipe index.
    pub report: AnalysisReport,
}

/// Knobs for [`run_and_analyze_with`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AnalyzeOptions {
    /// macOS only: inject Seatbelt `(trace …)` and write a draft allow profile here.
    ///
    /// **Permissive** — a traced run is not confined. Explicit opt-in only; the
    /// CLI gates it behind `--author`. Ignored on other platforms.
    pub author_trace: Option<PathBuf>,
}

/// A unique temp path for an `--author` trace profile.
pub fn default_trace_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "isol8-analyze-trace-{}-{}.sbpl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ))
}

/// Run `spec` confined, observe denials, and match them against recipes.
///
/// Always spawns (best-effort): a command that fails for its own reasons still
/// produces a report. Denials come from, in order:
///
/// 1. `ISOL8_ANALYZE_FEED` — an explicit NDJSON file (offline / CI / tests)
/// 2. the platform observer (macOS unified log; none elsewhere yet)
/// 3. a post-run `$TMP/isol8-analyze-<pid>.ndjson` written by a backend hook
///
/// Observation is **non-exhaustive** on every platform;
/// [`AnalysisReport::source_note`] records which source was used.
pub fn run_and_analyze(spec: &Spec, ctx: &Context) -> Result<AnalyzeOutcome> {
    run_and_analyze_with(spec, ctx, &AnalyzeOptions::default())
}

/// [`run_and_analyze`] with explicit options (`--author` trace).
pub fn run_and_analyze_with(
    spec: &Spec,
    ctx: &Context,
    opts: &AnalyzeOptions,
) -> Result<AnalyzeOutcome> {
    crate::sandbox::ensure_not_nested()?;

    let mut effective = crate::resolve::effective_policy_in(spec, ctx)?;
    crate::home::materialize(&effective.home)?;
    crate::resolve::confine_executable(&mut effective.profile, &mut effective.cmd)?;

    if let Some(path) = &opts.author_trace {
        author_trace(&mut effective.profile, path);
    }

    // An explicit feed wins over live observation, so a test or a CI run is
    // deterministic and never depends on the host log daemon.
    let (code, pid, denials, source_note) = match resolve_feed_path(None) {
        Some(path) => {
            let (code, pid) = spawn_and_wait(&effective)?;
            let d = load_ndjson_file(&path)?;
            (code, pid, d, format!("NDJSON feed {}", path.display()))
        }
        None => collect_denials_live(&effective)?,
    };

    // A backend hook may have written a per-pid feed only after the child ran.
    let (denials, source_note) = if denials.is_empty() {
        match resolve_feed_path(Some(pid)) {
            Some(path) => match load_ndjson_file(&path) {
                Ok(d) if !d.is_empty() => (d, format!("NDJSON feed {}", path.display())),
                _ => (denials, source_note),
            },
            None => (denials, source_note),
        }
    } else {
        (denials, source_note)
    };

    let run_ctx = RunContext::from_cmd(&effective.cmd);
    let reg = RecipeRegistry::load(&spec.recipe_paths)?;
    let index = build_recipe_index(&reg, &run_ctx, ctx, &effective.home.path)?;
    let report = analyze(&denials, &index, ctx, &effective.home.path, source_note);
    Ok(AnalyzeOutcome { code, pid, report })
}

/// Append a Seatbelt `(trace "path")` directive to the merged profile (macOS).
///
/// The traced process runs **permissively** — Seatbelt records what it touches
/// instead of denying. Never call this on a run whose confinement matters.
#[cfg(target_os = "macos")]
pub fn author_trace(profile: &mut crate::profile::Profile, trace_path: &Path) {
    let directive = crate::analyze_macos::seatbelt_trace_directive(trace_path);
    let macos = profile.macos.get_or_insert_with(Default::default);
    macos.raw.push_str(&directive);
}

/// No-op off macOS: Seatbelt `(trace …)` has no equivalent on other backends.
#[cfg(not(target_os = "macos"))]
pub fn author_trace(_profile: &mut crate::profile::Profile, _trace_path: &Path) {}

/// Spawn the confined command and wait, returning `(exit_code, pid)`.
fn spawn_and_wait(effective: &crate::resolve::EffectivePolicy) -> Result<(i32, u32)> {
    let backend = crate::backends::select();
    let mut child = backend.spawn(&effective.profile, &effective.env, &effective.cmd)?;
    let pid = child.id();
    let code = child.wait().unwrap_or(1);
    Ok((code, pid))
}

/// Spawn while the platform observer is running.
fn collect_denials_live(
    effective: &crate::resolve::EffectivePolicy,
) -> Result<(i32, u32, Vec<Denial>, String)> {
    #[cfg(target_os = "macos")]
    {
        match crate::analyze_macos::observe_denials_during(|| spawn_and_wait(effective)) {
            Ok((code, pid, denials)) => {
                let note = if denials.is_empty() {
                    no_observation_note().to_string()
                } else {
                    format!(
                        "macOS unified log (log stream + log show; {} denial line(s); pid={pid})",
                        denials.len()
                    )
                };
                Ok((code, pid, denials, note))
            }
            // The observer is best-effort: if the log daemon is unavailable the
            // command still runs, it just runs unobserved.
            Err(e) => {
                let (code, pid) = spawn_and_wait(effective)?;
                Ok((
                    code,
                    pid,
                    Vec::new(),
                    format!("log observer unavailable: {e}"),
                ))
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let (code, pid) = spawn_and_wait(effective)?;
        Ok((code, pid, Vec::new(), no_observation_note().to_string()))
    }
}
