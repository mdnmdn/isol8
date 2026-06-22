//! Windows AppContainer backend (Tier 1).
//!
//! Renders the merged Profile into an AppContainer policy and runs the command under
//! AppContainer confinement using the documented non-admin launch path:
//!   CreateAppContainerProfile → DeriveAppContainerSidFromAppContainerName (or reuse SID)
//!   → SECURITY_CAPABILITIES + PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES
//!   → CreateProcessW with EXTENDED_STARTUPINFO_PRESENT.
//!
//! Path grants from profiles are **documentary only** (R2 is partial on Windows in Phase 5);
//! AppContainer gives deny-by-default + capability-gated libraries, not fine-grained ACLs
//! unless the caller separately ACLs objects for the derived package SID.
//!
//! TODO(Phase 5+): Tier 2 elevated, Tier 3 Job+LowIL, resource limits, WFP net.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::sandbox::SandboxChild;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    AllocateAndInitializeSid, FreeSid, PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
    SID_IDENTIFIER_AUTHORITY,
};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    UpdateProcThreadAttribute, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    STARTUPINFOEXW,
};

use super::Backend;
use crate::home::expand_windows_vars;
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

        // launch now returns a live SandboxChild (non-blocking)
        let child = launch_appcontainer(&caps, env, cmd);
        free_capability_sids(&caps);
        child
    }

    fn render_policy(&self, profile: &Profile) -> String {
        render_policy(profile)
    }
}

fn launch_appcontainer(
    caps: &[SID_AND_ATTRIBUTES],
    env: &HashMap<String, String>,
    cmd: &[String],
) -> Result<SandboxChild> {
    // Unique name for this invocation's AppContainer profile (no admin).
    let container_name = format!(
        "Isol8.{:x}{:x}{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u32)
            .unwrap_or(0),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );

    let name_wide: Vec<u16> = container_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let name_pcw = PCWSTR(name_wide.as_ptr());

    let display = windows::core::w!("isol8");
    let desc = windows::core::w!("isol8 agent sandbox");

    // 1. Create profile (or derive if name already registered by previous run).
    let pkg_sid: PSID = unsafe {
        let cap_slice: Option<&[SID_AND_ATTRIBUTES]> =
            if caps.is_empty() { None } else { Some(caps) };
        match CreateAppContainerProfile(name_pcw, display, desc, cap_slice) {
            Ok(sid) => sid,
            Err(e) => {
                // 0x800700b7 == HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS) as i32
                if e.code().0 != (0x8007_00b7u32 as i32) {
                    return Err(Error::Message(format!(
                        "CreateAppContainerProfile({container_name}) failed: {e}"
                    )));
                }
                DeriveAppContainerSidFromAppContainerName(name_pcw).map_err(|e| {
                    Error::Message(format!(
                        "DeriveAppContainerSidFromAppContainerName({container_name}): {e}"
                    ))
                })?
            }
        }
    };

    // 2. SECURITY_CAPABILITIES struct for the attribute.
    let sec_caps = SECURITY_CAPABILITIES {
        AppContainerSid: pkg_sid,
        Capabilities: if caps.is_empty() {
            std::ptr::null_mut()
        } else {
            caps.as_ptr() as *mut SID_AND_ATTRIBUTES
        },
        CapabilityCount: caps.len() as u32,
        Reserved: 0,
    };

    // 3. Proc thread attribute list (size query + init + update).
    let mut attr_size: usize = 0;
    unsafe {
        // Query size (expected to "fail" with buffer size written).
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut attr_size);
    }
    if attr_size == 0 {
        attr_size = 2048;
    }
    let mut attr_buf: Vec<u8> = vec![0u8; attr_size];
    let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);

    unsafe {
        InitializeProcThreadAttributeList(Some(attr_list), 1, None, &mut attr_size)
            .map_err(|e| Error::Message(format!("InitializeProcThreadAttributeList: {e}")))?;

        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            Some(&sec_caps as *const _ as *const c_void),
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            None,
            None,
        )
        .map_err(|e| {
            Error::Message(format!(
                "UpdateProcThreadAttribute(SECURITY_CAPABILITIES): {e}"
            ))
        })?;
    }

    // 4. Quoted cmdline + env. When argv[0] is absolute, pass it as lpApplicationName
    // so CreateProcessW does not rely on PATH search inside the AppContainer.
    let cmd_line = build_quoted_command_line(cmd);
    let mut cmd_wide: Vec<u16> = cmd_line.encode_utf16().chain(std::iter::once(0)).collect();
    let app_wide: Option<Vec<u16>> = std::path::Path::new(&cmd[0])
        .is_absolute()
        .then(|| cmd[0].encode_utf16().chain(std::iter::once(0)).collect());
    let env_block = build_env_block(env);

    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.lpAttributeList = attr_list;

    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let creation_flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT;

    let app_name = app_wide
        .as_ref()
        .map(|w| PCWSTR(w.as_ptr()))
        .unwrap_or(PCWSTR(std::ptr::null()));

    let create_res = unsafe {
        CreateProcessW(
            app_name,
            Some(PWSTR(cmd_wide.as_mut_ptr())),
            None,
            None,
            false,
            creation_flags,
            Some(env_block.as_ptr() as *const c_void),
            PCWSTR(std::ptr::null()),
            &si.StartupInfo,
            &mut pi,
        )
    };

    // Always cleanup the attribute list we allocated.
    unsafe {
        DeleteProcThreadAttributeList(attr_list);
    }
    // Free the SID we received from Create/Derive (kernel took a copy for the token).
    free_pkg_sid(pkg_sid);

    if let Err(e) = create_res {
        return Err(Error::Message(format!(
            "CreateProcessW under AppContainer failed: {e}"
        )));
    }

    unsafe {
        let _ = CloseHandle(pi.hThread);
    }

    Ok(SandboxChild::windows(
        pi.dwProcessId,
        pi.hProcess,
        Some(container_name),
    ))
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn free_pkg_sid(sid: PSID) {
    if !sid.0.is_null() {
        unsafe {
            let _ = FreeSid(sid);
        }
    }
}

fn build_env_block(env: &HashMap<String, String>) -> Vec<u16> {
    if env.is_empty() {
        // A valid empty environment block is two terminating nulls.
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

/// Quote a single argument for the Windows command line parser (CommandLineToArgvW rules).
/// Rules: if the arg contains space, tab, or quote (or is empty), wrap in "..." .
/// Internal " become \", backslashes immediately before a " or at end are doubled.
fn quote_arg(arg: &str) -> String {
    if arg.is_empty() || arg.contains([' ', '\t', '"']) {
        let mut out = String::with_capacity(arg.len() + 2);
        out.push('"');
        let mut backslashes = 0usize;
        for ch in arg.chars() {
            if ch == '\\' {
                backslashes += 1;
            } else if ch == '"' {
                // emit doubled backs + \"
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
        // trailing backs before the closing quote must be doubled
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

fn free_capability_sids(caps: &[SID_AND_ATTRIBUTES]) {
    for cap in caps {
        if !cap.Sid.0.is_null() {
            unsafe {
                FreeSid(cap.Sid);
            }
        }
    }
}

pub fn render_policy(profile: &Profile) -> String {
    let mut out = String::new();
    out.push_str("-- Windows AppContainer policy (Tier 1) --\n");
    out.push_str("  Mechanism: AppContainer + SECURITY_CAPABILITIES (no admin)\n");
    out.push_str(
        "  Deny-by-default for most named objects, IPC, devices, and WinRT outside granted caps.\n",
    );
    out.push_str("  NOTE: filesystem path grants below are DOCUMENTARY ONLY and NOT ENFORCED.\n");
    out.push_str("        AppContainer does not support Seatbelt/Landlock-style per-path ro/rw.\n");
    out.push_str(
        "        To actually protect paths you must separately grant the derived package SID\n",
    );
    out.push_str(
        "        via icacls (defeats the policy-only model) or move tools under %ProgramFiles%.\n",
    );
    if let Some(w) = &profile.windows {
        if !w.capabilities.is_empty() {
            out.push_str("  Granted capabilities:\n");
            for c in &w.capabilities {
                out.push_str(&format!("    {c:?}\n"));
            }
        } else {
            out.push_str("  Granted capabilities: (none)\n");
        }
    } else {
        out.push_str("  Granted capabilities: (none)\n");
    }
    out.push_str("  Path grants (DOCUMENTARY / NOT ENFORCED):\n");
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
        let s = String::from_utf16(
            &block[..block.len().saturating_sub(1)]
                .iter()
                .copied()
                .collect::<Vec<_>>(),
        )
        .unwrap();
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
        // Trailing backslash alone does not require quoting (no space/tab/quote).
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

/// Delete a named AppContainer profile (best-effort).
/// Called by SandboxChild::wait / kill after the child has exited.
#[allow(dead_code)] // called via re-export from sandbox under cfg(windows)
pub(crate) fn delete_app_container_profile(name: &str) -> Result<()> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        // Ignore failure — profile may already be gone or still referenced briefly.
        let _ = DeleteAppContainerProfile(PCWSTR(wide.as_ptr()));
    }
    Ok(())
}
