//! Stage and inject `isol8-winhook.dll` into a suspended AppContainer child.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateRemoteThread, GetExitCodeThread, ResumeThread, WaitForSingleObject, INFINITE,
    LPTHREAD_START_ROUTINE,
};

use crate::error::{Error, Result};
use isol8_path_policy::PathPolicy;

const HOOK_DLL_NAME: &str = "isol8-winhook.dll";

/// Paths to search for the hook DLL (next to isol8 binary, then cwd).
pub fn hook_dll_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        let mut depth = 0u8;
        while let Some(d) = dir {
            paths.push(d.join(HOOK_DLL_NAME));
            depth = depth.saturating_add(1);
            if depth >= 3 {
                break;
            }
            dir = d.parent();
        }
    }
    paths.push(PathBuf::from(HOOK_DLL_NAME));
    paths
}

pub fn hook_dll_available() -> Option<PathBuf> {
    // Prefer the outermost match (e.g. `target/debug/`) over a stale copy beside a
    // unit-test binary in `target/debug/deps/`.
    hook_dll_search_paths()
        .into_iter()
        .rev()
        .find(|p| p.is_file())
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
            None,
            size,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );

        if remote.is_null() {
            return Err(Error::Message("VirtualAllocEx failed".into()));
        }

        WriteProcessMemory(process, remote, wide.as_ptr() as *const c_void, size, None)
            .map_err(|e| Error::Message(format!("WriteProcessMemory: {e}")))?;

        let kernel32 = GetModuleHandleW(windows::core::w!("kernel32.dll"))
            .map_err(|e| Error::Message(format!("GetModuleHandleW(kernel32): {e}")))?;
        let load_lib = GetProcAddress(HMODULE(kernel32.0), windows::core::s!("LoadLibraryW"))
            .ok_or_else(|| Error::Message("GetProcAddress(LoadLibraryW) failed".into()))?;

        let start: LPTHREAD_START_ROUTINE = Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        >(load_lib));
        let remote_thread = CreateRemoteThread(process, None, 0, start, Some(remote), 0, None)
            .map_err(|e| Error::Message(format!("CreateRemoteThread(LoadLibraryW): {e}")))?;

        let _ = WaitForSingleObject(remote_thread, INFINITE);

        let mut exit_code = 0u32;
        GetExitCodeThread(remote_thread, &mut exit_code)
            .map_err(|e| Error::Message(format!("GetExitCodeThread: {e}")))?;
        let _ = CloseHandle(remote_thread);
        let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);

        if exit_code == 0 {
            return Err(Error::Message(format!(
                "LoadLibraryW failed for {}",
                dll_path.display()
            )));
        }

        if ResumeThread(thread) == u32::MAX {
            return Err(Error::Message("ResumeThread failed".into()));
        }
        let _ = CloseHandle(thread);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_dll_name_is_stable() {
        assert_eq!(HOOK_DLL_NAME, "isol8-winhook.dll");
    }
}
