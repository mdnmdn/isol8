//! User-mode hook DLL injected into confined children to enforce isol8 path
//! grants (hybrid model: hook + optional AppContainer). See
//! `_docs/inbox/windows-policy-approach.md`.

#![allow(non_snake_case)]

use std::cell::Cell;
use std::ffi::c_void;
use std::fs;
use std::sync::OnceLock;

use isol8_path_policy::PathPolicy;
use minhook::{MinHook, MH_STATUS};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileA, CreateFileW, FindFirstFileA, FindFirstFileW, FILE_CREATION_DISPOSITION,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, WIN32_FIND_DATAA, WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleA, GetProcAddress,
};
use windows_sys::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows_sys::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};
use windows_sys::Win32::System::Threading::{
    CreateProcessA, CreateRemoteThread, GetExitCodeThread, ResumeThread, TerminateProcess,
    WaitForSingleObject, CREATE_SUSPENDED, INFINITE, LPTHREAD_START_ROUTINE, PROCESS_INFORMATION,
    STARTUPINFOA, STARTUPINFOW,
};

const STATUS_ACCESS_DENIED: i32 = 0xC0000022u32 as i32;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *const UnicodeString,
    attributes: u32,
    security_descriptor: *const c_void,
    security_quality_of_service: *const c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status: i32,
    information: usize,
}

type CreateFileWFn = unsafe extern "system" fn(
    *const u16,
    u32,
    FILE_SHARE_MODE,
    *const SECURITY_ATTRIBUTES,
    FILE_CREATION_DISPOSITION,
    FILE_FLAGS_AND_ATTRIBUTES,
    HANDLE,
) -> HANDLE;

type CreateFileAFn = unsafe extern "system" fn(
    *const u8,
    u32,
    FILE_SHARE_MODE,
    *const SECURITY_ATTRIBUTES,
    FILE_CREATION_DISPOSITION,
    FILE_FLAGS_AND_ATTRIBUTES,
    HANDLE,
) -> HANDLE;

type FindFirstFileWFn = unsafe extern "system" fn(*const u16, *mut WIN32_FIND_DATAW) -> HANDLE;
type FindFirstFileAFn = unsafe extern "system" fn(*const u8, *mut WIN32_FIND_DATAA) -> HANDLE;

type NtCreateFileFn = unsafe extern "system" fn(
    *mut HANDLE,
    u32,
    *const ObjectAttributes,
    *mut IoStatusBlock,
    *const i64,
    u32,
    u32,
    u32,
    u32,
    *const c_void,
    u32,
) -> i32;

static POLICY: OnceLock<PathPolicy> = OnceLock::new();
static ORIGINAL_W: OnceLock<CreateFileWFn> = OnceLock::new();
static ORIGINAL_A: OnceLock<CreateFileAFn> = OnceLock::new();
static ORIGINAL_FIND_W: OnceLock<FindFirstFileWFn> = OnceLock::new();
static ORIGINAL_FIND_A: OnceLock<FindFirstFileAFn> = OnceLock::new();
static ORIGINAL_NT: OnceLock<NtCreateFileFn> = OnceLock::new();
static ORIGINAL_CREATE_PROCESS_A: OnceLock<CreateProcessAFn> = OnceLock::new();
static ORIGINAL_CREATE_PROCESS_INTERNAL_W: OnceLock<CreateProcessInternalWFn> = OnceLock::new();
static HOOK_DLL_PATH: OnceLock<String> = OnceLock::new();

type CreateProcessAFn = unsafe extern "system" fn(
    *const u8,
    *mut u8,
    *const SECURITY_ATTRIBUTES,
    *const SECURITY_ATTRIBUTES,
    i32,
    u32,
    *const c_void,
    *const u8,
    *const STARTUPINFOA,
    *mut PROCESS_INFORMATION,
) -> i32;

/// `kernelbase!CreateProcessInternalW` — the real sink behind `kernel32!CreateProcessW`
/// (often a tiny forwarder too small for MinHook).
type CreateProcessInternalWFn = unsafe extern "system" fn(
    HANDLE,
    *const u16,
    *mut u16,
    *const SECURITY_ATTRIBUTES,
    *const SECURITY_ATTRIBUTES,
    i32,
    u32,
    *const c_void,
    *const u16,
    *const STARTUPINFOW,
    *mut PROCESS_INFORMATION,
    *mut HANDLE,
) -> i32;

thread_local! {
    /// Non-zero while a Win32 `CreateFile*` detour is calling the original (which
    /// reaches `NtCreateFile`). Skipping the Nt hook avoids double-filtering the
    /// same open with different access masks.
    static IN_WIN32_CREATE_FILE: Cell<u32> = const { Cell::new(0) };
}

struct Win32CreateGuard;

impl Win32CreateGuard {
    fn enter() -> Self {
        IN_WIN32_CREATE_FILE.with(|c| c.set(c.get().saturating_add(1)));
        Self
    }
}

impl Drop for Win32CreateGuard {
    fn drop(&mut self) {
        IN_WIN32_CREATE_FILE.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

fn in_win32_create_file() -> bool {
    IN_WIN32_CREATE_FILE.with(|c| c.get() > 0)
}

#[no_mangle]
pub unsafe extern "system" fn DllMain(
    module: *mut c_void,
    reason: u32,
    _reserved: *mut c_void,
) -> i32 {
    match reason {
        DLL_PROCESS_ATTACH => {
            if store_hook_dll_path(module).is_err()
                || load_policy().is_err()
                || install_hooks().is_err()
            {
                return 0;
            }
        }
        DLL_PROCESS_DETACH => {
            let _ = MinHook::disable_all_hooks();
        }
        _ => {}
    }
    1
}

fn store_hook_dll_path(module: *mut c_void) -> Result<(), ()> {
    let mut wide = [0u16; 32_768];
    let len = unsafe { GetModuleFileNameW(module, wide.as_mut_ptr(), wide.len() as u32) };
    if len == 0 {
        return Err(());
    }
    let path = String::from_utf16(&wide[..len as usize]).map_err(|_| ())?;
    HOOK_DLL_PATH.set(path).map_err(|_| ())
}

fn load_policy() -> Result<(), ()> {
    let body = if let Ok(inline) = std::env::var(PathPolicy::ENV_VAR) {
        inline
    } else if let Ok(path) = std::env::var(PathPolicy::ENV_FILE_VAR) {
        fs::read_to_string(&path).map_err(|_| ())?
    } else {
        return Err(());
    };
    let policy = PathPolicy::from_json(&body).map_err(|_| ())?;
    POLICY.set(policy).map_err(|_| ())
}

fn install_hooks() -> Result<(), ()> {
    unsafe {
        let orig_w =
            MinHook::create_hook(CreateFileW as _, create_file_w_detour as _).map_err(to_unit)?;
        ORIGINAL_W
            .set(std::mem::transmute(orig_w))
            .map_err(|_| ())?;
        let orig_a =
            MinHook::create_hook(CreateFileA as _, create_file_a_detour as _).map_err(to_unit)?;
        ORIGINAL_A
            .set(std::mem::transmute(orig_a))
            .map_err(|_| ())?;
        let orig_fw = MinHook::create_hook(FindFirstFileW as _, find_first_file_w_detour as _)
            .map_err(to_unit)?;
        ORIGINAL_FIND_W
            .set(std::mem::transmute(orig_fw))
            .map_err(|_| ())?;
        let orig_fa = MinHook::create_hook(FindFirstFileA as _, find_first_file_a_detour as _)
            .map_err(to_unit)?;
        ORIGINAL_FIND_A
            .set(std::mem::transmute(orig_fa))
            .map_err(|_| ())?;

        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr() as *const u8);
        if !ntdll.is_null() {
            let nt = GetProcAddress(ntdll, c"NtCreateFile".as_ptr() as *const u8);
            if let Some(nt) = nt {
                let orig_nt =
                    MinHook::create_hook(nt as _, nt_create_file_detour as _).map_err(to_unit)?;
                ORIGINAL_NT
                    .set(std::mem::transmute(orig_nt))
                    .map_err(|_| ())?;
            }
        }

        let orig_cpa = MinHook::create_hook(CreateProcessA as _, create_process_a_detour as _)
            .map_err(to_unit)?;
        ORIGINAL_CREATE_PROCESS_A
            .set(std::mem::transmute(orig_cpa))
            .map_err(|_| ())?;

        // `kernel32!CreateProcessW` is often a tiny forwarder; hook the real sink in kernelbase.
        let kernelbase = GetModuleHandleA(c"kernelbase.dll".as_ptr() as *const u8);
        if kernelbase.is_null() {
            return Err(());
        }
        let internal = GetProcAddress(kernelbase, c"CreateProcessInternalW".as_ptr() as *const u8);
        let Some(internal) = internal else {
            return Err(());
        };
        let orig_internal =
            MinHook::create_hook(internal as _, create_process_internal_w_detour as _)
                .map_err(to_unit)?;
        ORIGINAL_CREATE_PROCESS_INTERNAL_W
            .set(std::mem::transmute(orig_internal))
            .map_err(|_| ())?;

        MinHook::enable_all_hooks().map_err(to_unit)?;
    }
    Ok(())
}

fn to_unit(status: MH_STATUS) {
    let _ = status;
}

/// Remote `LoadLibraryW` inject into a suspended child, then resume if requested.
fn inject_hook_dll(process: HANDLE, thread: HANDLE, resume: bool) -> bool {
    let Some(dll_path) = HOOK_DLL_PATH.get() else {
        let _ = unsafe { TerminateProcess(process, 1) };
        return false;
    };
    let wide: Vec<u16> = dll_path.encode_utf16().chain(std::iter::once(0)).collect();
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
            let _ = TerminateProcess(process, 1);
            return false;
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
            let _ = TerminateProcess(process, 1);
            return false;
        }

        let kernel32 = GetModuleHandleA(c"kernel32.dll".as_ptr() as *const u8);
        if kernel32.is_null() {
            let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            let _ = TerminateProcess(process, 1);
            return false;
        }
        let load_lib = GetProcAddress(kernel32, c"LoadLibraryW".as_ptr() as *const u8);
        let Some(load_lib) = load_lib else {
            let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            let _ = TerminateProcess(process, 1);
            return false;
        };

        let start: LPTHREAD_START_ROUTINE = Some(std::mem::transmute(load_lib));
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
            let _ = TerminateProcess(process, 1);
            return false;
        }

        let _ = WaitForSingleObject(remote_thread, INFINITE);
        let mut exit_code = 0u32;
        if GetExitCodeThread(remote_thread, &mut exit_code) == 0 {
            let _ = TerminateProcess(process, 1);
            return false;
        }
        let _ = windows_sys::Win32::Foundation::CloseHandle(remote_thread);
        let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);

        if exit_code == 0 {
            let _ = TerminateProcess(process, 1);
            return false;
        }

        if resume && ResumeThread(thread) == u32::MAX {
            let _ = TerminateProcess(process, 1);
            return false;
        }
    }
    true
}

fn confine_created_process(
    lp_process_information: *mut PROCESS_INFORMATION,
    caller_suspended: bool,
) -> i32 {
    let pi = unsafe { &*lp_process_information };
    if !inject_hook_dll(pi.hProcess, pi.hThread, !caller_suspended) {
        unsafe {
            windows_sys::Win32::Foundation::SetLastError(ERROR_ACCESS_DENIED);
        }
        return 0;
    }
    1
}

unsafe extern "system" fn create_process_internal_w_detour(
    h_token: HANDLE,
    lp_application_name: *const u16,
    lp_command_line: *mut u16,
    lp_process_attributes: *const SECURITY_ATTRIBUTES,
    lp_thread_attributes: *const SECURITY_ATTRIBUTES,
    b_inherit_handles: i32,
    dw_creation_flags: u32,
    lp_environment: *const c_void,
    lp_current_directory: *const u16,
    lp_startup_info: *const STARTUPINFOW,
    lp_process_information: *mut PROCESS_INFORMATION,
    h_new_token: *mut HANDLE,
) -> i32 {
    let Some(original) = ORIGINAL_CREATE_PROCESS_INTERNAL_W.get() else {
        return 0;
    };
    // Only confine ordinary user creates (no explicit token).
    if !h_token.is_null() && h_token != INVALID_HANDLE_VALUE {
        return unsafe {
            original(
                h_token,
                lp_application_name,
                lp_command_line,
                lp_process_attributes,
                lp_thread_attributes,
                b_inherit_handles,
                dw_creation_flags,
                lp_environment,
                lp_current_directory,
                lp_startup_info,
                lp_process_information,
                h_new_token,
            )
        };
    }
    let caller_suspended = (dw_creation_flags & CREATE_SUSPENDED) != 0;
    let flags = dw_creation_flags | CREATE_SUSPENDED;
    let ok = unsafe {
        original(
            h_token,
            lp_application_name,
            lp_command_line,
            lp_process_attributes,
            lp_thread_attributes,
            b_inherit_handles,
            flags,
            lp_environment,
            lp_current_directory,
            lp_startup_info,
            lp_process_information,
            h_new_token,
        )
    };
    if ok == 0 {
        return 0;
    }
    confine_created_process(lp_process_information, caller_suspended)
}

unsafe extern "system" fn create_process_a_detour(
    lp_application_name: *const u8,
    lp_command_line: *mut u8,
    lp_process_attributes: *const SECURITY_ATTRIBUTES,
    lp_thread_attributes: *const SECURITY_ATTRIBUTES,
    b_inherit_handles: i32,
    dw_creation_flags: u32,
    lp_environment: *const c_void,
    lp_current_directory: *const u8,
    lp_startup_info: *const STARTUPINFOA,
    lp_process_information: *mut PROCESS_INFORMATION,
) -> i32 {
    let Some(original) = ORIGINAL_CREATE_PROCESS_A.get() else {
        return 0;
    };
    let caller_suspended = (dw_creation_flags & CREATE_SUSPENDED) != 0;
    let flags = dw_creation_flags | CREATE_SUSPENDED;
    let ok = unsafe {
        original(
            lp_application_name,
            lp_command_line,
            lp_process_attributes,
            lp_thread_attributes,
            b_inherit_handles,
            flags,
            lp_environment,
            lp_current_directory,
            lp_startup_info,
            lp_process_information,
        )
    };
    if ok == 0 {
        return 0;
    }
    confine_created_process(lp_process_information, caller_suspended)
}

/// Write-related bits only. Do not use `FILE_GENERIC_WRITE` (0x120116): it includes
/// `SYNCHRONIZE`, which overlaps legitimate read opens (`GENERIC_READ | SYNCHRONIZE`).
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_WRITE_DATA: u32 = 0x0002;
const FILE_APPEND_DATA: u32 = 0x0004;
const FILE_WRITE_EA: u32 = 0x0010;
const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
const DELETE: u32 = 0x0001_0000;
const WRITE_MASK: u32 = GENERIC_WRITE
    | FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_WRITE_EA
    | FILE_WRITE_ATTRIBUTES
    | DELETE;

/// Win32 `CreateFile*` disposition values (`OPEN_EXISTING` = 3).
fn wants_write_win32(desired_access: u32, disposition: u32) -> bool {
    const OPEN_EXISTING: u32 = 3;
    if disposition == OPEN_EXISTING {
        return (desired_access & WRITE_MASK) != 0;
    }
    (desired_access & WRITE_MASK) != 0 || matches!(disposition, 1 | 2 | 4 | 5)
}

/// `NtCreateFile` disposition values (`FILE_OPEN` = 1, `FILE_OPEN_IF` = 3).
fn wants_write_nt(desired_access: u32, disposition: u32) -> bool {
    if matches!(disposition, 1 | 3) {
        return (desired_access & WRITE_MASK) != 0;
    }
    (desired_access & WRITE_MASK) != 0 || matches!(disposition, 0 | 2 | 4 | 5)
}

fn deny_handle() -> HANDLE {
    unsafe {
        windows_sys::Win32::Foundation::SetLastError(ERROR_ACCESS_DENIED);
    }
    INVALID_HANDLE_VALUE
}

fn user_path(raw: &str) -> String {
    let mut p = raw.replace('/', "\\");
    for prefix in [r"\\?\", r"\??\", r"\DosDevices\"] {
        if let Some(rest) = p.strip_prefix(prefix) {
            p = rest.to_string();
            break;
        }
    }
    p
}

fn allowed(raw: &str, write: bool) -> bool {
    let path = user_path(raw);
    POLICY
        .get()
        .map(|policy| policy.allows(&path, write))
        .unwrap_or(false)
}

fn check_path(raw: &str, desired_access: u32, disposition: u32, is_nt: bool) -> bool {
    let path = user_path(raw);
    let Some(policy) = POLICY.get() else {
        return false;
    };
    let wants_write = if is_nt {
        wants_write_nt(desired_access, disposition)
    } else {
        wants_write_win32(desired_access, disposition)
    };
    policy.allows(&path, wants_write)
}

fn path_from_object_attributes(attrs: *const ObjectAttributes) -> Option<String> {
    if attrs.is_null() {
        return None;
    }
    let attrs = unsafe { &*attrs };
    if attrs.object_name.is_null() {
        return None;
    }
    let name = unsafe { &*attrs.object_name };
    if name.buffer.is_null() || name.length == 0 {
        return None;
    }
    let chars = name.length as usize / 2;
    let wide = unsafe { std::slice::from_raw_parts(name.buffer, chars) };
    String::from_utf16(wide).ok()
}

unsafe extern "system" fn nt_create_file_detour(
    file_handle: *mut HANDLE,
    desired_access: u32,
    object_attributes: *const ObjectAttributes,
    io_status_block: *mut IoStatusBlock,
    allocation_size: *const i64,
    file_attributes: u32,
    share_access: u32,
    create_disposition: u32,
    create_options: u32,
    ea_buffer: *const c_void,
    ea_length: u32,
) -> i32 {
    if !in_win32_create_file() {
        if let Some(path) = path_from_object_attributes(object_attributes) {
            if !check_path(&path, desired_access, create_disposition, true) {
                return STATUS_ACCESS_DENIED;
            }
        }
    }
    let Some(original) = ORIGINAL_NT.get() else {
        return STATUS_ACCESS_DENIED;
    };
    unsafe {
        original(
            file_handle,
            desired_access,
            object_attributes,
            io_status_block,
            allocation_size,
            file_attributes,
            share_access,
            create_disposition,
            create_options,
            ea_buffer,
            ea_length,
        )
    }
}

unsafe extern "system" fn create_file_w_detour(
    lp_file_name: *const u16,
    desired_access: u32,
    share_mode: FILE_SHARE_MODE,
    security_attributes: *const SECURITY_ATTRIBUTES,
    creation_disposition: FILE_CREATION_DISPOSITION,
    flags_and_attributes: FILE_FLAGS_AND_ATTRIBUTES,
    template_file: HANDLE,
) -> HANDLE {
    if !lp_file_name.is_null() {
        let wide = unsafe { std::slice::from_raw_parts(lp_file_name, wide_len(lp_file_name)) };
        if let Ok(path) = String::from_utf16(wide) {
            if !check_path(&path, desired_access, creation_disposition, false) {
                return deny_handle();
            }
        }
    }
    let Some(original) = ORIGINAL_W.get() else {
        return INVALID_HANDLE_VALUE;
    };
    let _guard = Win32CreateGuard::enter();
    unsafe {
        original(
            lp_file_name,
            desired_access,
            share_mode,
            security_attributes,
            creation_disposition,
            flags_and_attributes,
            template_file,
        )
    }
}

unsafe extern "system" fn create_file_a_detour(
    lp_file_name: *const u8,
    desired_access: u32,
    share_mode: FILE_SHARE_MODE,
    security_attributes: *const SECURITY_ATTRIBUTES,
    creation_disposition: FILE_CREATION_DISPOSITION,
    flags_and_attributes: FILE_FLAGS_AND_ATTRIBUTES,
    template_file: HANDLE,
) -> HANDLE {
    if !lp_file_name.is_null() {
        let narrow = unsafe { std::slice::from_raw_parts(lp_file_name, narrow_len(lp_file_name)) };
        let path = String::from_utf8_lossy(narrow);
        if !check_path(&path, desired_access, creation_disposition, false) {
            return deny_handle();
        }
    }
    let Some(original) = ORIGINAL_A.get() else {
        return INVALID_HANDLE_VALUE;
    };
    let _guard = Win32CreateGuard::enter();
    unsafe {
        original(
            lp_file_name,
            desired_access,
            share_mode,
            security_attributes,
            creation_disposition,
            flags_and_attributes,
            template_file,
        )
    }
}

fn normalize_dir_pattern(path: &str) -> String {
    let mut p = path.replace('/', "\\");
    if let Some(stripped) = p.strip_suffix("\\*") {
        p = stripped.to_string();
    } else if let Some(stripped) = p.strip_suffix('*') {
        p = stripped.trim_end_matches('\\').to_string();
    }
    while p.len() > 3 && p.ends_with('\\') {
        p.pop();
    }
    p
}

unsafe extern "system" fn find_first_file_w_detour(
    lp_file_name: *const u16,
    find_data: *mut WIN32_FIND_DATAW,
) -> HANDLE {
    if !lp_file_name.is_null() {
        let wide = unsafe { std::slice::from_raw_parts(lp_file_name, wide_len(lp_file_name)) };
        if let Ok(pattern) = String::from_utf16(wide) {
            let dir = normalize_dir_pattern(&pattern);
            if !allowed(&dir, false) {
                return deny_handle();
            }
        }
    }
    let Some(original) = ORIGINAL_FIND_W.get() else {
        return INVALID_HANDLE_VALUE;
    };
    unsafe { original(lp_file_name, find_data) }
}

unsafe extern "system" fn find_first_file_a_detour(
    lp_file_name: *const u8,
    find_data: *mut WIN32_FIND_DATAA,
) -> HANDLE {
    if !lp_file_name.is_null() {
        let narrow = unsafe { std::slice::from_raw_parts(lp_file_name, narrow_len(lp_file_name)) };
        let pattern = String::from_utf8_lossy(narrow);
        let dir = normalize_dir_pattern(&pattern);
        if !allowed(&dir, false) {
            return deny_handle();
        }
    }
    let Some(original) = ORIGINAL_FIND_A.get() else {
        return INVALID_HANDLE_VALUE;
    };
    unsafe { original(lp_file_name, find_data) }
}

fn wide_len(ptr: *const u16) -> usize {
    let mut len = 0usize;
    unsafe {
        while *ptr.add(len) != 0 {
            len += 1;
            if len > 32_768 {
                break;
            }
        }
    }
    len
}

fn narrow_len(ptr: *const u8) -> usize {
    let mut len = 0usize;
    unsafe {
        while *ptr.add(len) != 0 {
            len += 1;
            if len > 32_768 {
                break;
            }
        }
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_read_with_synchronize_is_not_write() {
        const GENERIC_READ: u32 = 0x8000_0000;
        const SYNCHRONIZE: u32 = 0x0010_0000;
        const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
        let access = GENERIC_READ | SYNCHRONIZE | FILE_READ_ATTRIBUTES;
        assert!(!wants_write_nt(access, 1));
        assert!(!wants_write_win32(access, 3));
    }

    #[test]
    fn generic_write_is_write() {
        assert!(wants_write_nt(0x4000_0000, 1));
        assert!(wants_write_win32(0x4000_0000, 3));
    }

    #[test]
    fn create_disposition_implies_write_without_read_bits() {
        assert!(wants_write_nt(0, 2));
        assert!(wants_write_win32(0, 2));
    }

    #[test]
    fn delete_access_is_write() {
        assert!(wants_write_nt(DELETE, 1));
        assert!(wants_write_win32(DELETE, 3));
    }

    #[test]
    fn read_ea_is_not_write() {
        assert!(!wants_write_nt(0x0008, 1));
        assert!(!wants_write_win32(0x0008, 3));
    }
}
