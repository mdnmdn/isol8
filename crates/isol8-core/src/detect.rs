//! Toolchain detection and cage verification (evolution Phase 4).
//!
//! - **Detect** is read-only: `stat` on each recipe's `detect.probe` path (and an
//!   optional trusted version command on the **host**).
//! - **Verify** materializes the cage home, then runs each recipe's `verify.cmd`
//!   *inside* the sandbox (`sandbox::run_captured`).
//!
//! See evo-repo §6.2 / §6.5 and `_docs/wip/multi-evo-plan.md` Phase 4.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};
use crate::filter::RunContext;
use crate::home::{expand_tilde, REAL_HOME_TOKEN};
use crate::recipe::{Recipe, RecipeRegistry, ToolchainChoice};
use crate::sandbox::Spec;

/// Outcome of probing one recipe against the real home.
#[derive(Debug, Clone)]
pub struct DetectResult {
    /// Recipe id.
    pub id: String,
    /// One-line summary.
    pub summary: String,
    /// True when the probe path exists (or no probe configured — treated as unknown).
    pub found: bool,
    /// Expanded probe path that was checked (if any).
    pub probe_path: Option<PathBuf>,
    /// Optional version string from `detect.version` (host command).
    pub version: Option<String>,
    /// Why version was skipped (untrusted source, missing cmd, spawn failure, …).
    pub version_note: Option<String>,
    /// Recipe source label (`builtin:…` or path).
    pub source: String,
}

/// Outcome of verifying one recipe's smoke test inside a cage.
#[derive(Debug, Clone)]
pub struct VerifyResult {
    /// Recipe id.
    pub id: String,
    /// Strategy used for this cage choice.
    pub strategy: String,
    /// True when exit code 0 and optional expect regex matched.
    pub ok: bool,
    /// Human detail (stdout snippet, error, skip reason).
    pub detail: String,
    /// Suggested fix when not ok.
    pub fix_hint: Option<String>,
}

/// True when `detect.version` / `verify.cmd` may run for this recipe source.
///
/// Trusted:
/// - `builtin:…`
/// - local filesystem paths
/// - `registry:<trust>:<name>:<id>` when `<trust>` is `official` or `local`
///
/// Untrusted: raw URLs, `registry:community:…`, `registry:untrusted:…`.
pub fn commands_trusted(source: &str) -> bool {
    if source.starts_with("builtin:") {
        return true;
    }
    if source.contains("://") {
        return false;
    }
    if let Some(rest) = source.strip_prefix("registry:") {
        // Format: registry:<trust>:<name>:<id>  (Phase 7)
        // Legacy / shorthand: registry:<name>:… → untrusted until trust known
        let trust = rest.split(':').next().unwrap_or("untrusted");
        return matches!(trust, "official" | "local");
    }
    true
}

/// Expand a detect probe path against the **real** home (`~` and `#HOME` both → real).
pub fn expand_probe_path(raw: &str, real_home: &Path) -> PathBuf {
    let s = if raw.contains(REAL_HOME_TOKEN) {
        raw.replace(REAL_HOME_TOKEN, &real_home.to_string_lossy())
    } else {
        raw.to_string()
    };
    PathBuf::from(expand_tilde(&s, real_home))
}

/// Probe a single recipe (read-only).
pub fn detect_recipe(recipe: &Recipe, real_home: &Path) -> DetectResult {
    let (found, probe_path) = match &recipe.detect.probe_path {
        Some(p) => {
            let path = expand_probe_path(p, real_home);
            let found = path.exists();
            (found, Some(path))
        }
        None => (false, None),
    };

    let (version, version_note) = match &recipe.detect.version_cmd {
        None => (None, None),
        Some(_) if !found && recipe.detect.probe_path.is_some() => {
            (None, Some("skipped (probe path missing)".into()))
        }
        Some(cmd) if !commands_trusted(&recipe.source) => (
            None,
            Some(format!(
                "skipped (untrusted source {}; Phase 7 trust gate)",
                recipe.source
            )),
        ),
        Some(cmd) => match run_host_command(cmd) {
            Ok(out) => {
                let line = out.lines().next().unwrap_or("").trim().to_string();
                if line.is_empty() {
                    (Some("(empty)".into()), None)
                } else {
                    (Some(line), None)
                }
            }
            Err(e) => (None, Some(format!("version cmd failed: {e}"))),
        },
    };

    DetectResult {
        id: recipe.id.clone(),
        summary: recipe.summary.clone(),
        found,
        probe_path,
        version,
        version_note,
        source: recipe.source.clone(),
    }
}

/// Detect all recipes that match the platform (and have a probe or are listed).
///
/// Recipes without a probe are still listed as "no probe" so authors see them.
pub fn detect_all(
    reg: &RecipeRegistry,
    ctx: &RunContext,
    real_home: &Path,
) -> Result<Vec<DetectResult>> {
    detect_all_with(reg, ctx, real_home, None)
}

/// Progress callback: `(index_1based, total, short_id)`.
pub type DetectProgress<'a> = dyn FnMut(usize, usize, &str) + 'a;

/// Like [`detect_all`], with optional progress: `(index_1based, total, short_id)`.
///
/// Called once per recipe **before** that recipe is probed. `short_id` strips a
/// leading `toolchains/` prefix when present (e.g. `nvm`).
pub fn detect_all_with(
    reg: &RecipeRegistry,
    ctx: &RunContext,
    real_home: &Path,
    mut on_progress: Option<&mut DetectProgress<'_>>,
) -> Result<Vec<DetectResult>> {
    // Resolve first so total is known before any probe (and for stable progress).
    let mut recipes = Vec::new();
    for id in reg.ids() {
        match reg.resolve(&id, ctx) {
            Ok(recipe) => recipes.push(recipe),
            Err(_) => continue, // platform mismatch — omit from detect list
        }
    }
    let total = recipes.len();
    let mut out = Vec::with_capacity(total);
    for (i, recipe) in recipes.into_iter().enumerate() {
        let short = recipe
            .id
            .strip_prefix("toolchains/")
            .unwrap_or(recipe.id.as_str());
        if let Some(cb) = on_progress.as_mut() {
            cb(i + 1, total, short);
        }
        out.push(detect_recipe(recipe, real_home));
    }
    Ok(out)
}

/// Format detect results for CLI output.
pub fn format_detect_table(results: &[DetectResult]) -> String {
    let mut s = String::from("Detected in ~:\n");
    if results.is_empty() {
        s.push_str("  (no recipes for this platform)\n");
        return s;
    }
    for r in results {
        let mark = if r.probe_path.is_none() {
            "·"
        } else if r.found {
            "✓"
        } else {
            "·"
        };
        let path = r
            .probe_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no probe)".into());
        let short = r.id.strip_prefix("toolchains/").unwrap_or(&r.id);
        s.push_str(&format!("  {mark} {short:<12} {path}"));
        if let Some(v) = &r.version {
            s.push_str(&format!("  ({v})"));
        } else if let Some(n) = &r.version_note {
            s.push_str(&format!("  [{n}]"));
        } else if !r.found && r.probe_path.is_some() {
            s.push_str("  not found");
        }
        s.push('\n');
    }
    s
}

/// Verify every toolchain choice on `spec` by running `verify.cmd` inside the cage.
///
/// Materializes once (via the first captured run's pipeline); each recipe gets its
/// own sandboxed command. Recipes without `verify.cmd` are reported as skipped.
pub fn verify_toolchains(spec: &Spec) -> Result<Vec<VerifyResult>> {
    let ctx = RunContext::from_cmd(&spec.cmd);
    let reg = RecipeRegistry::load(&spec.recipe_paths)?;
    let mut results = Vec::new();

    if spec.toolchains.is_empty() {
        return Ok(vec![VerifyResult {
            id: "(none)".into(),
            strategy: String::new(),
            ok: true,
            detail: "cage has no [toolchains.*] entries".into(),
            fix_hint: Some(
                "add [toolchains.<id>] strategy = \"…\" to the cage, then re-run verify".into(),
            ),
        }]);
    }

    // Materialize home once before individual verifies.
    {
        let mut materialize_spec = spec.clone();
        materialize_spec.cmd = vec!["true".into()]; // placeholder; not executed for plan-only
                                                    // Build plan + apply without running a command.
        let ambient = crate::context::Context::from_environment()?;
        let layers = resolve_layers_for_materialize(&materialize_spec)?;
        let mut home_spec = materialize_spec.clone();
        let contributions = reg.compile_all(&spec.toolchains, &ctx)?;
        for c in &contributions {
            home_spec.home_ops.extend(c.home_ops.iter().cloned());
        }
        let home = crate::home::resolve(&home_spec, &layers, &ambient)?;
        crate::home::materialize(&home)?;
        results.push(VerifyResult {
            id: "home".into(),
            strategy: String::new(),
            ok: true,
            detail: format!("materialized {}", home.path.display()),
            fix_hint: None,
        });
    }

    for choice in &spec.toolchains {
        results.push(verify_one(spec, &reg, choice, &ctx)?);
    }
    Ok(results)
}

fn resolve_layers_for_materialize(spec: &Spec) -> Result<Vec<crate::profile::Profile>> {
    use crate::filter;
    use crate::profile::{self, LayerRegistry};
    let registry = LayerRegistry::load(&spec.profile_paths)?;
    let ctx = RunContext::from_cmd(&spec.cmd);
    let selected = profile::select_layer_names(spec, &registry, &ctx)?;
    let all = registry.profiles();
    let resolved = profile::resolve_requires(&selected, &all)?;
    Ok(resolved
        .into_iter()
        .map(|(_, p)| filter::apply_layer_filter(p, &ctx))
        .collect())
}

fn verify_one(
    base: &Spec,
    reg: &RecipeRegistry,
    choice: &ToolchainChoice,
    ctx: &RunContext,
) -> Result<VerifyResult> {
    let recipe = match reg.resolve(&choice.id, ctx) {
        Ok(r) => r,
        Err(e) => {
            return Ok(VerifyResult {
                id: choice.id.clone(),
                strategy: choice.strategy.as_str().into(),
                ok: false,
                detail: e.to_string(),
                fix_hint: None,
            });
        }
    };

    let Some(verify_cmd) = recipe.verify.cmd.as_ref() else {
        return Ok(VerifyResult {
            id: recipe.id.clone(),
            strategy: choice.strategy.as_str().into(),
            ok: true,
            detail: "no verify.cmd (skipped)".into(),
            fix_hint: None,
        });
    };

    if !commands_trusted(&recipe.source) {
        return Ok(VerifyResult {
            id: recipe.id.clone(),
            strategy: choice.strategy.as_str().into(),
            ok: false,
            detail: format!(
                "verify.cmd blocked: untrusted source {} (Phase 7 trust gate)",
                recipe.source
            ),
            fix_hint: Some(
                "only builtin and local recipes may run verify until registry trust lands".into(),
            ),
        });
    }

    let mut run_spec = base.clone();
    run_spec.cmd = shell_command(verify_cmd);
    // Avoid re-seeding issues; materialize already ran.
    // Keep toolchains so grants/env stay applied.
    run_spec.no_seed = true;

    let captured = match crate::sandbox::run_captured(run_spec) {
        Ok(c) => c,
        Err(e) => {
            return Ok(VerifyResult {
                id: recipe.id.clone(),
                strategy: choice.strategy.as_str().into(),
                ok: false,
                detail: format!("launch failed: {e}"),
                fix_hint: Some(format!(
                    "inspect with: isol8 -c <cage> --show-policies -- {verify_cmd}"
                )),
            });
        }
    };

    let stdout = captured.stdout.trim();
    let stderr = captured.stderr.trim();
    let combined = if stderr.is_empty() {
        stdout.to_string()
    } else if stdout.is_empty() {
        stderr.to_string()
    } else {
        format!("{stdout}\n{stderr}")
    };

    let mut ok = captured.code == 0;
    let mut detail = if combined.is_empty() {
        format!("exit {}", captured.code)
    } else {
        let first = combined.lines().next().unwrap_or("").trim();
        format!("exit {} → {first}", captured.code)
    };

    if ok {
        if let Some(pat) = &recipe.verify.expect {
            match regex_is_match(pat, &combined) {
                Ok(true) => {}
                Ok(false) => {
                    ok = false;
                    detail = format!(
                        "exit 0 but output did not match expect {pat:?}: {}",
                        combined.lines().next().unwrap_or("")
                    );
                }
                Err(e) => {
                    ok = false;
                    detail = format!("invalid expect regex {pat:?}: {e}");
                }
            }
        }
    }

    let fix_hint = if ok {
        None
    } else {
        Some(format!(
            "try: isol8 -c <cage> --show-policies -- {verify_cmd}\n\
             or add a missing path grant / switch strategy (see _docs/recipes.md)"
        ))
    };

    Ok(VerifyResult {
        id: recipe.id.clone(),
        strategy: choice.strategy.as_str().into(),
        ok,
        detail,
        fix_hint,
    })
}

/// Format verify results for CLI.
pub fn format_verify_report(results: &[VerifyResult]) -> String {
    let mut s = String::new();
    for r in results {
        let mark = if r.ok { "✓" } else { "✗" };
        if r.id == "home" {
            s.push_str(&format!("  {mark} home             {}\n", r.detail));
            continue;
        }
        let short = r.id.strip_prefix("toolchains/").unwrap_or(&r.id);
        let strat = if r.strategy.is_empty() {
            String::new()
        } else {
            format!("[{}] ", r.strategy)
        };
        s.push_str(&format!("  {mark} {short:<12} {strat}{}\n", r.detail));
        if let Some(fix) = &r.fix_hint {
            for line in fix.lines() {
                s.push_str(&format!("             {line}\n"));
            }
        }
    }
    let failed = results.iter().filter(|r| !r.ok).count();
    if failed == 0 {
        s.push_str("\nall checks passed\n");
    } else {
        s.push_str(&format!("\n{failed} check(s) failed\n"));
    }
    s
}

fn shell_command(cmd: &str) -> Vec<String> {
    if cfg!(windows) {
        return vec!["cmd.exe".into(), "/C".into(), cmd.to_string()];
    }
    // Prefer argv split when the command has no shell metacharacters so we do
    // not depend on /bin/sh being granted (and avoid an extra hop).
    let needs_shell = cmd
        .chars()
        .any(|c| matches!(c, '|' | '>' | '<' | '&' | ';' | '$' | '`' | '\n'));
    if needs_shell {
        vec!["/bin/sh".into(), "-c".into(), cmd.to_string()]
    } else {
        cmd.split_whitespace().map(str::to_string).collect()
    }
}

fn run_host_command(cmd: &str) -> Result<String> {
    let output = if cfg!(windows) {
        Command::new("cmd.exe").args(["/C", cmd]).output()
    } else {
        Command::new("/bin/sh").args(["-c", cmd]).output()
    }
    .map_err(|e| Error::Message(format!("failed to run {cmd:?}: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        let msg = stderr.trim();
        return Err(Error::Message(if msg.is_empty() {
            format!("command exited {}", output.status)
        } else {
            msg.to_string()
        }));
    }
    Ok(stdout)
}

/// Tiny expect matcher (no `regex` crate). Supports `^`/`$`, literals, `.`, and
/// `\d` / `\d+` / `\d*` — enough for recipe patterns like `^v\d+`.
fn regex_is_match(pattern: &str, text: &str) -> Result<bool> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut pat = pattern.trim();
    let from_start = if let Some(rest) = pat.strip_prefix('^') {
        pat = rest;
        true
    } else {
        false
    };
    let to_end = if let Some(rest) = pat.strip_suffix('$') {
        pat = rest;
        true
    } else {
        false
    };

    enum Tok {
        Lit(String),
        /// (`min`, `greedy`) — greedy consumes all following digits when true.
        Digits {
            min: usize,
            greedy: bool,
        },
        Any,
    }

    let mut toks: Vec<Tok> = Vec::new();
    let mut lit = String::new();
    let b = pat.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() && b[i + 1] != b'd' {
            // Escaped metacharacter (`\.`, `\\`, `\$`) → literal.
            lit.push(b[i + 1] as char);
            i += 2;
            continue;
        }
        if b[i] == b'\\' && i + 1 < b.len() && b[i + 1] == b'd' {
            if !lit.is_empty() {
                toks.push(Tok::Lit(std::mem::take(&mut lit)));
            }
            i += 2;
            let (min, greedy) = if i < b.len() && b[i] == b'+' {
                i += 1;
                (1, true)
            } else if i < b.len() && b[i] == b'*' {
                i += 1;
                (0, true)
            } else {
                (1, false) // bare \d → exactly one digit
            };
            toks.push(Tok::Digits { min, greedy });
            continue;
        }
        if b[i] == b'.' {
            if !lit.is_empty() {
                toks.push(Tok::Lit(std::mem::take(&mut lit)));
            }
            toks.push(Tok::Any);
            i += 1;
            continue;
        }
        // Anything else regex-ish would silently degrade to a literal — say so instead.
        if matches!(
            b[i],
            b'(' | b')' | b'|' | b'[' | b']' | b'{' | b'}' | b'?' | b'+' | b'*'
        ) {
            return Err(Error::Message(format!(
                "unsupported construct '{}' (supported: ^ $ . \\d \\d+ \\d* and literals; \
                 escape with \\{})",
                b[i] as char, b[i] as char
            )));
        }
        lit.push(b[i] as char);
        i += 1;
    }
    if !lit.is_empty() {
        toks.push(Tok::Lit(lit));
    }

    // Match the token list against `tb` starting exactly at `start`.
    let match_at = |tb: &[u8], start: usize| -> bool {
        let mut pos = start;
        for tok in &toks {
            match tok {
                Tok::Lit(s) => {
                    let sb = s.as_bytes();
                    if pos + sb.len() > tb.len() || &tb[pos..pos + sb.len()] != sb {
                        return false;
                    }
                    pos += sb.len();
                }
                Tok::Any => {
                    if pos >= tb.len() {
                        return false;
                    }
                    pos += 1;
                }
                Tok::Digits { min, greedy } => {
                    let at = pos;
                    if *greedy {
                        while pos < tb.len() && tb[pos].is_ascii_digit() {
                            pos += 1;
                        }
                    } else if pos < tb.len() && tb[pos].is_ascii_digit() {
                        pos += 1;
                    }
                    if pos - at < *min {
                        return false;
                    }
                }
            }
        }
        !to_end || pos == tb.len()
    };

    for line in lines {
        let tb = line.as_bytes();
        // Unanchored patterns search the line; `^` pins the match to offset 0.
        let last = if from_start { 0 } else { tb.len() };
        if (0..=last).any(|s| match_at(tb, s)) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::real_home_from_env;
    use crate::recipe::parse_recipe;

    #[test]
    fn probe_hit_and_miss() {
        let tmp = std::env::temp_dir().join(format!("isol8-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".tool")).unwrap();

        let recipe = parse_recipe(
            r##"
schema = 1
id = "toolchains/demo"
kind = "recipe"
[detect]
probe = { path = "~/.tool" }
[strategies.isolate]
home = [{ kind = "mkdir", path = "~/.tool" }]
paths = [{ path = "~/.tool", access = "rw" }]
"##,
            "test",
        )
        .unwrap();

        let hit = detect_recipe(&recipe, &tmp);
        assert!(hit.found);
        assert!(hit.probe_path.unwrap().ends_with(".tool"));

        let miss_home = tmp.join("empty");
        std::fs::create_dir_all(&miss_home).unwrap();
        let miss = detect_recipe(&recipe, &miss_home);
        assert!(!miss.found);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn untrusted_source_blocks_version() {
        let recipe = parse_recipe(
            r##"
schema = 1
id = "toolchains/x"
kind = "recipe"
[detect]
probe = { path = "~/.x" }
version = "echo 1.0"
[strategies.isolate]
paths = []
"##,
            "registry:https://evil.example/x",
        )
        .unwrap();
        // Create probe so version would run if trusted.
        let tmp = std::env::temp_dir().join(format!("isol8-detect-u-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".x")).unwrap();
        let r = detect_recipe(&recipe, &tmp);
        assert!(r.found);
        assert!(r.version.is_none());
        assert!(r
            .version_note
            .as_deref()
            .is_some_and(|n| n.contains("untrusted")));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn expect_v_digits() {
        assert!(regex_is_match(r"^v\d+", "v22.3.0").unwrap());
        assert!(!regex_is_match(r"^v\d+", "nope").unwrap());
    }

    #[test]
    fn detect_all_with_reports_progress() {
        let dir = std::env::temp_dir().join(format!("isol8-detect-prog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("demo.toml"),
            r##"
schema = 1
id = "toolchains/demo-prog"
kind = "recipe"
[detect]
probe = { path = "~/.isol8-detect-progress-probe" }
[strategies.isolate]
paths = []
"##,
        )
        .unwrap();
        let reg =
            RecipeRegistry::load_with_registry_dirs(&[], &[(String::new(), dir.clone())]).unwrap();
        let ctx = RunContext::from_cmd(&[]);
        let real = real_home_from_env();
        let mut seen: Vec<(usize, usize, String)> = Vec::new();
        let mut cb = |i: usize, total: usize, name: &str| {
            seen.push((i, total, name.to_string()));
        };
        let rows = detect_all_with(&reg, &ctx, &real, Some(&mut cb)).unwrap();
        // load_with_registry_dirs still loads builtins; filter to our id.
        assert!(
            seen.iter()
                .any(|(i, t, n)| *i >= 1 && *t >= 1 && n == "demo-prog"),
            "progress should mention demo-prog: {seen:?}"
        );
        assert!(rows.iter().any(|r| r.id == "toolchains/demo-prog"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expect_escaped_dot_is_literal() {
        assert!(regex_is_match(r"^\d+\.", "1.3.4").unwrap());
        assert!(regex_is_match(r"^cargo \d+\.", "cargo 1.96.0 (30a34c6)").unwrap());
        assert!(regex_is_match(r"\d+\.\d+\.\d+", "8.0.100 [/usr/share]").unwrap());
        // `\.` must not behave like `.`
        assert!(!regex_is_match(r"^\d+\.", "1x3").unwrap());
    }

    #[test]
    fn expect_is_unanchored_and_multiline() {
        // No `^` → search anywhere in the line.
        assert!(regex_is_match("version", r#"openjdk version "21.0.3" LTS"#).unwrap());
        assert!(regex_is_match(r"Gradle \d+\.", "-----\n\nGradle 8.7\n\n").unwrap());
        // `^` still pins to the start of a line.
        assert!(!regex_is_match(r"^Gradle", "the Gradle build").unwrap());
        assert!(regex_is_match(r"^/opt/homebrew$", "/opt/homebrew").unwrap());
    }

    #[test]
    fn expect_rejects_unsupported_constructs() {
        let e = regex_is_match(r"^/(opt/homebrew|usr/local)$", "/opt/homebrew").unwrap_err();
        assert!(e.to_string().contains("unsupported construct"), "{e}");
    }

    #[test]
    fn commands_trusted_rules() {
        assert!(commands_trusted("builtin:toolchains/nvm"));
        assert!(commands_trusted("/home/u/.config/isol8/recipes/x.toml"));
        assert!(commands_trusted(
            "registry:official:fixture:toolchains/sample"
        ));
        assert!(commands_trusted("registry:local:scratch:toolchains/x"));
        assert!(!commands_trusted("registry:community:work:toolchains/x"));
        assert!(!commands_trusted("registry:official/foo")); // legacy shorthand
        assert!(!commands_trusted("https://example.com/r.toml"));
    }

    #[test]
    fn expand_probe_uses_real_home() {
        let p = expand_probe_path("~/.nvm", Path::new("/Users/me"));
        assert_eq!(p, PathBuf::from("/Users/me/.nvm"));
        let p = expand_probe_path("#HOME/.nvm", Path::new("/Users/me"));
        assert_eq!(p, PathBuf::from("/Users/me/.nvm"));
        let _ = real_home_from_env();
    }
}
