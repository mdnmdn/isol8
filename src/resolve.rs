//! Shared effective-policy resolution pipeline for run and introspection commands.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::cli::RunArgs;
use crate::env;
use crate::filter::RunContext;
use crate::home::{self, EffectiveHome};
use crate::profile::{self, Access, LayerRegistry, MatchKind, PathGrant, Profile};

/// Fully resolved policy for a confined command.
pub struct EffectivePolicy {
    pub layer_names: Vec<String>,
    pub profile: Profile,
    pub env: std::collections::HashMap<String, String>,
    pub home: EffectiveHome,
    /// The command after profile `rewrite` rules are applied (what actually runs).
    pub cmd: Vec<String>,
}

/// Resolve layer stack, home, merged profile, and env without spawning.
pub fn effective_policy(run: &RunArgs) -> Result<EffectivePolicy> {
    let registry = LayerRegistry::load(run.profile_paths())?;
    let ctx = RunContext::from_cmd(&run.cmd);
    let layer_names = profile::select_layer_names(run, &registry, &ctx)?;
    let all = registry.profiles();
    let layers = profile::resolve_requires(&layer_names, &all)?;
    let effective_home = home::resolve(run, &layers)?;
    let merged = profile::load_merged(run, &layers, &effective_home, &ctx)?;
    let env_map = env::build_minimal(&merged, &effective_home.path);
    let cmd = profile::apply_rewrite(&run.cmd, &merged.rewrite);
    Ok(EffectivePolicy {
        layer_names,
        profile: merged,
        env: env_map,
        home: effective_home,
        cmd,
    })
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
        cmd[0] = exe_str;
    }
    Ok(())
}

/// Resolve `name` to an absolute executable path, mirroring execvp: a name
/// containing `/` is treated as a path (relative to cwd), otherwise the host
/// `PATH` is searched. Returns a clean error if nothing executable is found.
fn resolve_executable(name: &str) -> Result<PathBuf> {
    if name.contains('/') {
        let p = Path::new(name);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()?.join(p)
        };
        if is_executable_file(&abs) {
            return Ok(abs);
        }
        bail!("command {name:?} not found");
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
    bail!("command {name:?} not found");
}

/// True if `p` exists, is a regular file (symlinks followed), and is executable.
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_path_command_to_absolute() {
        // /bin/sh exists and is executable on every unix host; point PATH at it so
        // the bare-name branch resolves regardless of the test runner's own PATH.
        std::env::set_var("PATH", "/bin");
        let p = resolve_executable("sh").unwrap();
        assert_eq!(p, PathBuf::from("/bin/sh"));
        assert!(is_executable_file(&p));
    }

    #[test]
    fn absolute_path_passes_through() {
        assert_eq!(
            resolve_executable("/bin/sh").unwrap(),
            PathBuf::from("/bin/sh")
        );
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
