//! Stage and inject `isol8-winhook.dll` into a suspended child process.
//!
//! The hook DLL is embedded at compile time (see `build.rs`) and materialized to a
//! cache file on first use so isol8 works as a library dependency without shipping a
//! separate DLL beside the consumer binary. A DLL found next to the isol8 binary still
//! wins for local development iteration.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::{
    CreateRemoteThread, GetExitCodeThread, ResumeThread, TerminateProcess, WaitForSingleObject,
    INFINITE, LPTHREAD_START_ROUTINE,
};

use crate::error::{Error, Result};
use isol8_path_policy::PathPolicy;

include!(concat!(env!("OUT_DIR"), "/winhook_embedded.rs"));

/// Stable on-disk name used for extracted and release-sidecar copies.
pub const HOOK_DLL_FILENAME: &str = "isol8-winhook.dll";

/// Cargo emits `isol8_winhook.dll`; `isol8-winhook.dll` is a legacy manual copy.
const HOOK_DLL_NAMES: &[&str] = &["isol8_winhook.dll", "isol8-winhook.dll"];

static MATERIALIZED_DLL: OnceLock<PathBuf> = OnceLock::new();

/// Paths to search for a developer-provided hook DLL (next to the isol8 binary only).
///
/// The search is intentionally restricted to the binary's own directory tree.
/// A bare filename fallback (CWD search) is omitted on purpose: it would allow
/// an attacker to place a malicious hook DLL in any directory the user navigates
/// to and have it injected into every sandboxed child.
pub fn hook_dll_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        let mut depth = 0u8;
        while let Some(d) = dir {
            for name in HOOK_DLL_NAMES {
                paths.push(d.join(name));
            }
            depth = depth.saturating_add(1);
            if depth >= 3 {
                break;
            }
            dir = d.parent();
        }
    }
    paths
}

fn hook_dll_on_disk() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = hook_dll_search_paths()
        .into_iter()
        .filter(|p| p.is_file())
        .collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| {
        let rank = |p: &PathBuf| {
            let underscore = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "isol8_winhook.dll");
            let depth = p.components().count();
            (depth, !underscore)
        };
        rank(a).cmp(&rank(b))
    });
    candidates.into_iter().next()
}

fn dll_cache_path(bytes: &[u8]) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    bytes.len().hash(&mut hasher);
    bytes.hash(&mut hasher);
    let tag = format!("{:016x}", hasher.finish());

    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TEMP").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    base.join("isol8")
        .join(format!("{HOOK_DLL_FILENAME}-{tag}"))
}

fn materialize_embedded_dll() -> Result<PathBuf> {
    if EMBEDDED_WINHOOK_DLL.is_empty() {
        return Err(Error::Message(
            "embedded isol8-winhook.dll is missing; rebuild isol8 for a Windows target".into(),
        ));
    }

    Ok(MATERIALIZED_DLL
        .get_or_init(|| {
            let path = dll_cache_path(EMBEDDED_WINHOOK_DLL);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create isol8 winhook cache dir");
            }
            if !path.is_file() {
                std::fs::write(&path, EMBEDDED_WINHOOK_DLL).expect("write embedded winhook dll");
            }
            path
        })
        .clone())
}

/// Resolve the hook DLL path.
///
/// Order: process `ISOL8_WINHOOK_DLL` → sidecar next to the host binary →
/// embedded payload materialized under `%LOCALAPPDATA%\isol8`.
pub fn resolve_hook_dll() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("ISOL8_WINHOOK_DLL") {
        let path = PathBuf::from(&p);
        if path.is_file() {
            return Ok(path);
        }
        return Err(Error::Message(format!(
            "ISOL8_WINHOOK_DLL points to a missing file: {p}"
        )));
    }
    if let Some(path) = hook_dll_on_disk() {
        return Ok(path);
    }
    materialize_embedded_dll()
}

/// Write policy JSON and return `(policy_file, staged_dll)` under `temp_dir`.
#[allow(dead_code)]
pub fn stage_policy_and_dll(
    temp_dir: &Path,
    policy: &PathPolicy,
    source_dll: &Path,
) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(temp_dir).map_err(Error::from)?;
    let tag = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .elapsed()
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let policy_path = temp_dir.join(format!("isol8-policy-{tag}.json"));
    let dll_path = temp_dir.join(format!("isol8-winhook-{tag}.dll"));
    std::fs::write(
        &policy_path,
        policy
            .to_json()
            .map_err(|e| Error::Message(e.to_string()))?,
    )
    .map_err(Error::from)?;
    std::fs::copy(source_dll, &dll_path).map_err(Error::from)?;
    Ok((policy_path, dll_path))
}

fn abort_suspended_child(process: HANDLE) {
    unsafe {
        let _ = TerminateProcess(process, 1);
    }
}

/// `LoadLibraryW` remote inject, then resume the primary thread.
pub fn inject_dll_and_resume(process: HANDLE, thread: HANDLE, dll_path: &Path) -> Result<()> {
    let wide: Vec<u16> = dll_path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let size = wide.len() * std::mem::size_of::<u16>();

    unsafe {
        let remote = VirtualAllocEx(
            process,
            std::ptr::null(),
            size,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );

        if remote.is_null() {
            abort_suspended_child(process);
            return Err(Error::Message(format!(
                "VirtualAllocEx failed: GetLastError={}",
                GetLastError()
            )));
        }

        if WriteProcessMemory(
            process,
            remote,
            wide.as_ptr() as *const c_void,
            size,
            std::ptr::null_mut(),
        ) == 0
        {
            let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            abort_suspended_child(process);
            return Err(Error::Message(format!(
                "WriteProcessMemory failed: GetLastError={}",
                GetLastError()
            )));
        }

        let kernel32 = GetModuleHandleA(c"kernel32.dll".as_ptr() as *const u8);
        if kernel32.is_null() {
            let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            abort_suspended_child(process);
            return Err(Error::Message(format!(
                "GetModuleHandleA(kernel32) failed: GetLastError={}",
                GetLastError()
            )));
        }
        let load_lib = GetProcAddress(kernel32, c"LoadLibraryW".as_ptr() as *const u8);
        let Some(load_lib) = load_lib else {
            let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            abort_suspended_child(process);
            return Err(Error::Message(format!(
                "GetProcAddress(LoadLibraryW) failed: GetLastError={}",
                GetLastError()
            )));
        };

        let start: LPTHREAD_START_ROUTINE = Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            unsafe extern "system" fn(*mut c_void) -> u32,
        >(load_lib));
        let remote_thread = CreateRemoteThread(
            process,
            std::ptr::null(),
            0,
            start,
            remote,
            0,
            std::ptr::null_mut(),
        );
        if remote_thread.is_null() {
            let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            abort_suspended_child(process);
            return Err(Error::Message(format!(
                "CreateRemoteThread(LoadLibraryW) failed: GetLastError={}",
                GetLastError()
            )));
        }

        let _ = WaitForSingleObject(remote_thread, INFINITE);

        let mut exit_code = 0u32;
        if GetExitCodeThread(remote_thread, &mut exit_code) == 0 {
            let _ = CloseHandle(remote_thread);
            let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            abort_suspended_child(process);
            return Err(Error::Message(format!(
                "GetExitCodeThread failed: GetLastError={}",
                GetLastError()
            )));
        }
        let _ = CloseHandle(remote_thread);
        let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);

        if exit_code == 0 {
            abort_suspended_child(process);
            return Err(Error::Message(format!(
                "LoadLibraryW failed for {}",
                dll_path.display()
            )));
        }

        if ResumeThread(thread) == u32::MAX {
            abort_suspended_child(process);
            return Err(Error::Message(format!(
                "ResumeThread failed: GetLastError={}",
                GetLastError()
            )));
        }
        let _ = CloseHandle(thread);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_dll_names_include_cargo_artifact() {
        assert!(HOOK_DLL_NAMES.contains(&"isol8_winhook.dll"));
    }

    #[test]
    fn hook_dll_filename_is_stable() {
        assert_eq!(HOOK_DLL_FILENAME, "isol8-winhook.dll");
    }

    #[test]
    fn embedded_winhook_payload_is_present() {
        assert!(
            !EMBEDDED_WINHOOK_DLL.is_empty(),
            "expected embedded winhook DLL bytes on Windows builds"
        );
    }

    #[test]
    fn hook_dll_search_paths_are_all_absolute() {
        let paths = hook_dll_search_paths();
        for p in &paths {
            assert!(
                p.is_absolute(),
                "hook_dll_search_paths() returned a non-absolute path: {:?}",
                p
            );
        }
    }

    #[test]
    fn resolve_hook_dll_returns_absolute_path() {
        let path = resolve_hook_dll().expect("resolve_hook_dll");
        assert!(path.is_absolute());
        assert!(path.is_file());
    }
}
