//! isol8 Windows user-mode hook DLL.
//!
//! On Windows this crate is a `cdylib` injected into confined children. On other
//! targets it compiles as an empty placeholder so the workspace still builds.

#[cfg(windows)]
#[allow(non_snake_case)]
mod winhook;

#[cfg(windows)]
pub use winhook::DllMain;

#[cfg(not(windows))]
#[allow(dead_code)]
fn isol8_winhook_non_windows_placeholder() {}
