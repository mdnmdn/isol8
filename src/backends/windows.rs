//! Windows AppContainer backend (token-based).
//!
//! Renders the merged Profile into an AppContainer policy and runs the command under
//! process-windowing confinement (no file ACLs are modified).
//!
//! ## Architecture
//!
//! **Tier 1 — AppContainer (primary).** Duplicates the process token, sets the
//! AppContainer SID and capability SIDs via `SetTokenInformation`, then launches
//! the command via `CreateProcessAsUserW`.
//!
//! TODO(Phase 5): Tier 2 — Elevated retry via `ShellExecuteExW("runas")`.
//! TODO(Phase 5): Tier 3 — Job Object + Low IL + Restricted Token fallback.
//! TODO(Phase 5): `--elevate` / `--no-elevate` CLI flags.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result, ResultExt};
use crate::sandbox::SandboxChild;
use windows::core::PCWSTR;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    AllocateAndInitializeSid, DuplicateTokenEx, FreeSid, SecurityImpersonation,
    TokenAppContainerSid, TokenCapabilities, SID_AND_ATTRIBUTES, SID_IDENTIFIER_AUTHORITY,
    TOKEN_ADJUST_DEFAULT, TOKEN_ALL_ACCESS, TOKEN_APPCONTAINER_INFORMATION, TOKEN_ASSIGN_PRIMARY,
    TOKEN_DUPLICATE, TOKEN_QUERY, TOKEN_TYPE,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken,
    WaitForSingleObject, PROCESS_INFORMATION, STARTUPINFOW,
};

use super::Backend;
use crate::profile::{Profile, WindowsCapability};

pub struct WindowsBackend;

/// AppContainer capability RID mapping.
const CAPABILITY_RIDS: &[(WindowsCapability, u32)] = &[
    (WindowsCapability::InternetClient, 1),
    (WindowsCapability::InternetClientServer, 2),
    (WindowsCapability::PrivateNetworkClientServer, 3),
    (WindowsCapability::PicturesLibrary, 4),
    (WindowsCapability::VideosLibrary, 5),
    (WindowsCapability::MusicLibrary, 6),
    (WindowsCapability::DocumentsLibrary, 7),
    (WindowsCapability::EnterpriseAuthentication, 8),
    (WindowsCapability::SharedUserCertificates, 9),
    (WindowsCapability::RemovableStorage, 10),
    (WindowsCapability::Appointments, 11),
    (WindowsCapability::Contacts, 12),
];

fn app_package_authority() -> SID_IDENTIFIER_AUTHORITY {
    SID_IDENTIFIER_AUTHORITY {
        Value: [0, 0, 0, 0, 0, 15],
    }
}

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

        let caps = build_capability_sids(
            profile
                .windows
                .as_ref()
                .map(|w| &w.capabilities)
                .unwrap_or(&Vec::new()),
        );

        let result = launch_appcontainer(&caps, env, cmd);
        free_capability_sids(&caps);
        result.map(SandboxChild::exited)
    }

    fn render_policy(&self, profile: &Profile) -> String {
        render_policy(profile)
    }
}

fn launch_appcontainer(
    caps: &[SID_AND_ATTRIBUTES],
    env: &HashMap<String, String>,
    cmd: &[String],
) -> Result<i32> {
    unsafe {
        let mut proc_token = HANDLE(std::ptr::null_mut());
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
            &mut proc_token as *mut HANDLE,
        )
        .ctx(|| "OpenProcessToken failed")?;

        let mut dup_token = HANDLE(std::ptr::null_mut());
        DuplicateTokenEx(
            proc_token,
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TOKEN_TYPE(1), // TokenPrimary
            &mut dup_token as *mut HANDLE,
        )
        .ctx(|| "DuplicateTokenEx failed")?;

        let _ = CloseHandle(proc_token);

        // Create unique AppContainer package SID: S-1-15-2-{BASE}-{U1}-{U2}-{U3}
        let mut pkg_sid = windows::Win32::Security::PSID(std::ptr::null_mut());
        let token = hex_token_values();
        AllocateAndInitializeSid(
            &app_package_authority() as *const SID_IDENTIFIER_AUTHORITY,
            4, // 4 subauthorities
            2, // SECURITY_APP_PACKAGE_BASE_RID
            token.0,
            token.1,
            token.2,
            0,
            0,
            0,
            0,
            &mut pkg_sid as *mut windows::Win32::Security::PSID,
        )
        .ctx(|| "AllocateAndInitializeSid (package SID) failed")?;

        // Set AppContainer SID on the token.
        let ac_info = TOKEN_APPCONTAINER_INFORMATION {
            TokenAppContainer: pkg_sid,
        };
        set_token_info(
            dup_token,
            TokenAppContainerSid,
            &ac_info as *const _ as *const c_void,
            std::mem::size_of::<TOKEN_APPCONTAINER_INFORMATION>() as u32,
        )
        .ctx(|| "SetTokenInformation(TokenAppContainerSid) failed")?;

        // Set capabilities on the token.
        if !caps.is_empty() {
            let caps_buf = build_token_groups(caps);
            set_token_info(
                dup_token,
                TokenCapabilities,
                caps_buf.as_ptr() as *const c_void,
                caps_buf.len() as u32,
            )
            .ctx(|| "SetTokenInformation(TokenCapabilities) failed")?;
        }

        // Build command line and env.
        let cmd_line = cmd.join(" ");
        let mut cmd_wide: Vec<u16> = cmd_line.encode_utf16().collect();
        cmd_wide.push(0);
        let env_block = build_env_block(env);

        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();

        CreateProcessAsUserW(
            Some(dup_token),
            PCWSTR(std::ptr::null()),
            Some(PWSTR(cmd_wide.as_mut_ptr())),
            None,
            None,
            false,
            Default::default(),
            Some(env_block.as_ptr() as *const c_void),
            None,
            &mut si as *mut STARTUPINFOW,
            &mut pi as *mut PROCESS_INFORMATION,
        )
        .ctx(|| "CreateProcessAsUserW failed")?;

        WaitForSingleObject(pi.hProcess, 0xFFFFFFFF);

        let mut exit_code: u32 = 0;
        GetExitCodeProcess(pi.hProcess, &mut exit_code as *mut u32)
            .ctx(|| "GetExitCodeProcess failed")?;

        let _ = CloseHandle(pi.hProcess);
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(dup_token);
        FreeSid(pkg_sid);

        Ok(exit_code as i32)
    }
}

fn set_token_info(
    token: HANDLE,
    info_class: windows::Win32::Security::TOKEN_INFORMATION_CLASS,
    value: *const c_void,
    value_len: u32,
) -> Result<()> {
    unsafe {
        windows::Win32::Security::SetTokenInformation(token, info_class, value, value_len)
            .ctx(|| "SetTokenInformation system call failed")
    }
}

fn build_env_block(env: &HashMap<String, String>) -> Vec<u16> {
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

fn build_capability_sids(caps: &[WindowsCapability]) -> Vec<SID_AND_ATTRIBUTES> {
    let authority = app_package_authority();
    caps.iter()
        .filter_map(|cap| {
            let rid = CAPABILITY_RIDS.iter().find(|(c, _)| c == cap)?.1;
            let mut sid = windows::Win32::Security::PSID(std::ptr::null_mut());
            let result = unsafe {
                AllocateAndInitializeSid(
                    &authority as *const SID_IDENTIFIER_AUTHORITY,
                    2,
                    3,
                    rid,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    &mut sid as *mut windows::Win32::Security::PSID,
                )
            };
            if result.is_err() || sid.0.is_null() {
                return None;
            }
            Some(SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: 4,
            }) // SE_GROUP_ENABLED
        })
        .collect()
}

fn build_token_groups(caps: &[SID_AND_ATTRIBUTES]) -> Vec<u8> {
    let header_size = std::mem::size_of::<u32>();
    let entry_size = std::mem::size_of::<SID_AND_ATTRIBUTES>();
    let total = header_size + std::mem::size_of_val(caps);
    let mut buf = vec![0u8; total];
    buf[..4].copy_from_slice(&(caps.len() as u32).to_ne_bytes());
    for (i, cap) in caps.iter().enumerate() {
        let offset = header_size + i * entry_size;
        let entry: [u8; std::mem::size_of::<SID_AND_ATTRIBUTES>()] =
            unsafe { std::mem::transmute(*cap) };
        buf[offset..offset + entry_size].copy_from_slice(&entry);
    }
    buf
}

fn free_capability_sids(caps: &[SID_AND_ATTRIBUTES]) {
    for cap in caps {
        if !cap.Sid.0.is_null() {
            unsafe {
                FreeSid(cap.Sid);
            }
        }
    }
}

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

pub fn render_policy(profile: &Profile) -> String {
    let mut out = String::new();
    out.push_str("-- Windows AppContainer policy --\n");
    out.push_str("  Deny-by-default: process runs under AppContainer token\n");
    if let Some(w) = &profile.windows {
        if !w.capabilities.is_empty() {
            out.push_str("  Capabilities:\n");
            for c in &w.capabilities {
                out.push_str(&format!("    {c:?}\n"));
            }
        }
    } else {
        out.push_str("  Capabilities: (none)\n");
    }
    out.push_str("  Path grants (documentary):\n");
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

fn hex_token_values() -> (u32, u32, u32) {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    (
        std::process::id(),
        (nanos >> 32) as u32,
        (nanos as u32).wrapping_add(COUNTER.fetch_add(1, Ordering::Relaxed)),
    )
}
