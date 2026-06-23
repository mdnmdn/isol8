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
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

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
    _module: *mut c_void,
    reason: u32,
    _reserved: *mut c_void,
) -> i32 {
    match reason {
        DLL_PROCESS_ATTACH => {
            if load_policy().is_err() || install_hooks().is_err() {
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

        MinHook::enable_all_hooks().map_err(to_unit)?;
    }
    Ok(())
}

fn to_unit(status: MH_STATUS) -> () {
    let _ = status;
}

/// Write-related bits only. Do not use `FILE_GENERIC_WRITE` (0x120116): it includes
/// `SYNCHRONIZE`, which overlaps legitimate read opens (`GENERIC_READ | SYNCHRONIZE`).
const GENERIC_WRITE: u32 = 0x4000_0000;
const WRITE_MASK: u32 = GENERIC_WRITE | 0x0002 | 0x0004 | 0x0008 | 0x0010 | 0x0100;

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
}
