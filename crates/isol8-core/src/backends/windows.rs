//! Windows hook backend — path grants enforced via `isol8-winhook.dll`.
//!
//! The hook DLL is embedded at compile time and materialized on first use (see
//! `windows_hook.rs`), so library embedders do not need a separate DLL artifact.
//!
//! TODO(Phase 5+): Job Object + Low IL, resource limits, WFP network tiers.

use std::collections::HashMap;
use std::ffi::c_void;

use crate::error::{Error, Result};
use crate::sandbox::SandboxChild;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTUPINFOW,
};

use super::windows_hook::{inject_dll_and_resume, resolve_hook_dll};
use super::windows_policy::path_policy_from_profile;
use super::Backend;
use crate::home::expand_windows_vars;
use crate::profile::Profile;
use isol8_path_policy::PathPolicy;

/// Windows hook-DLL backend (path grants enforced via `isol8-winhook.dll`).
pub struct WindowsBackend;

impl Backend for WindowsBackend {
    fn spawn(
        &self,
        profile: &Profile,
        env: &HashMap<String, String>,
        cmd: &[String],
    ) -> Result<SandboxChild> {
        if cmd.is_empty() {
            return Err(Error::Message(
                "no command given to run under the sandbox".into(),
            ));
        }

        let hook_dll = match env.get("ISOL8_WINHOOK_DLL") {
            Some(p) => {
                let path = std::path::PathBuf::from(p);
                if path.is_file() {
                    path
                } else {
                    return Err(Error::Message(format!(
                        "ISOL8_WINHOOK_DLL is not a file: {}",
                        path.display()
                    )));
                }
            }
            None => resolve_hook_dll()?,
        };
        launch_with_hook(&hook_dll, env, cmd, profile)
    }

    fn render_policy(&self, profile: &Profile) -> String {
        render_policy(profile)
    }
}

fn launch_with_hook(
    hook_dll: &std::path::Path,
    env: &HashMap<String, String>,
    cmd: &[String],
    profile: &Profile,
) -> Result<SandboxChild> {
    let policy = path_policy_from_profile(profile);
    let mut child_env = env.clone();
    child_env.remove("ISOL8_WINHOOK_DLL");
    child_env.insert(
        PathPolicy::ENV_VAR.to_string(),
        policy
            .to_json()
            .map_err(|e| Error::Message(e.to_string()))?,
    );

    let cmd_line = build_quoted_command_line(cmd);
    let mut cmd_wide: Vec<u16> = cmd_line.encode_utf16().chain(std::iter::once(0)).collect();
    let app_wide: Option<Vec<u16>> = std::path::Path::new(&cmd[0])
        .is_absolute()
        .then(|| cmd[0].encode_utf16().chain(std::iter::once(0)).collect());
    let env_block = build_env_block(&child_env);

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;

    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let creation_flags = CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED;
    let app_name = app_wide
        .as_ref()
        .map(|w| w.as_ptr())
        .unwrap_or(std::ptr::null());

    let ok = unsafe {
        CreateProcessW(
            app_name,
            cmd_wide.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            creation_flags,
            env_block.as_ptr() as *const c_void,
            std::ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        let code = unsafe { GetLastError() };
        return Err(Error::Message(format!(
            "CreateProcessW failed: GetLastError={code}"
        )));
    }

    inject_dll_and_resume(pi.hProcess, pi.hThread, hook_dll)?;

    Ok(SandboxChild::windows(pi.dwProcessId, pi.hProcess))
}

/// Path where the hook DLL may append NDJSON denials for `--analyze`.
pub fn analyze_denial_log_path(pid: u32) -> std::path::PathBuf {
    crate::analyze::default_denial_log_path(pid)
}

fn build_env_block(env: &HashMap<String, String>) -> Vec<u16> {
    if env.is_empty() {
        return vec![0, 0];
    }
    let mut buf = Vec::new();
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    for k in &keys {
        let v = &env[*k];
        for c in format!("{}={}", k, v).encode_utf16() {
            buf.push(c);
        }
        buf.push(0);
    }
    buf.push(0);
    buf
}

fn quote_arg(arg: &str) -> String {
    if arg.is_empty() || arg.contains([' ', '\t', '"']) {
        let mut out = String::with_capacity(arg.len() + 2);
        out.push('"');
        let mut backslashes = 0usize;
        for ch in arg.chars() {
            if ch == '\\' {
                backslashes += 1;
            } else if ch == '"' {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push('\\');
                out.push('"');
                backslashes = 0;
            } else {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push(ch);
                backslashes = 0;
            }
        }
        for _ in 0..backslashes {
            out.push('\\');
        }
        out.push('"');
        out
    } else {
        arg.to_owned()
    }
}

fn build_quoted_command_line(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Render the merged profile as hook-DLL policy text for dry-run / `--show-policies`.
pub fn render_policy(profile: &Profile) -> String {
    let hook = resolve_hook_dll().ok();
    let mut out = String::new();
    out.push_str("-- Windows hook policy (isol8-winhook.dll) --\n");
    if let Some(path) = &hook {
        out.push_str(&format!(
            "  Hook DLL: {} (embedded payload materialized on first use)\n",
            path.display()
        ));
        out.push_str("  Path grants: ENFORCED via CreateFile* / NtCreateFile hooks\n");
        out.push_str("  Policy transport: inline ISOL8_PATH_POLICY env JSON\n");
    } else {
        out.push_str("  Hook DLL: unavailable (rebuild isol8 for a Windows target)\n");
        out.push_str("  Path grants: NOT ENFORCED\n");
    }
    out.push_str("  Path grants:\n");
    if profile.paths.is_empty() {
        out.push_str("    (none)\n");
    } else {
        for g in &profile.paths {
            let exp = expand_windows_vars(&g.path);
            out.push_str(&format!("    {:?} {:?} {}", g.access, g.r#match, exp));
            if exp != g.path {
                out.push_str(&format!("  (from {})", g.path));
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_env_block_is_double_null() {
        let block = build_env_block(&HashMap::new());
        assert_eq!(block, vec![0u16, 0u16]);
    }

    #[test]
    fn env_block_encodes_sorted_entries() {
        let mut env = HashMap::new();
        env.insert("B".into(), "2".into());
        env.insert("A".into(), "1".into());
        let block = build_env_block(&env);
        let s = String::from_utf16(&block[..block.len().saturating_sub(1)]).unwrap();
        assert!(s.starts_with("A=1\0B=2"));
    }

    #[test]
    fn quote_arg_spaces_and_quotes() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(
            quote_arg("C:\\Program Files\\app.exe"),
            "\"C:\\Program Files\\app.exe\""
        );
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote_arg("trail\\"), "trail\\");
    }

    #[test]
    fn quoted_command_line_joins_args() {
        let line = build_quoted_command_line(&[
            "C:\\Program Files\\isol8.exe".into(),
            "--flag".into(),
            "a b".into(),
        ]);
        assert_eq!(line, "\"C:\\Program Files\\isol8.exe\" --flag \"a b\"");
    }
}
