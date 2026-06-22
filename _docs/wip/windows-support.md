# Windows Support — Implementation Notes

## Status

**Phase 5 (MVP).** Tier 1 AppContainer launch path implemented (`CreateAppContainerProfile`
+ `SECURITY_CAPABILITIES` + `CreateProcessW`). No admin required (policy-only — no file
ACLs modified). Path grants remain documentary (R2 partial). Verify with
`cargo run --bin isol8-field-test` on a Windows host.

## Three-Tier Architecture (planned)

| Tier | Mechanism | Admin? | Enforces |
|------|-----------|--------|---------|
| 1 | AppContainer + SECURITY_CAPABILITIES | No | Deny-by-default FS, IPC, device isolation |
| 2 | Elevated AppContainer (runas) | Yes | Same as T1, but retried via UAC |
| 3 | Job Object + Low IL + Restricted Token | No | Process tree teardown, write-restriction, privilege reduction |

**Current: Tier 1 only.** Tiers 2–3 have `TODO(Phase 5)` markers in the code.

## AppContainer Implementation (`backends/windows.rs`)

### Flow

1. Generate a unique container name (`Isol8.<hex>`)
2. Map `WindowsCapability` enum → well-known SID `S-1-15-3-{N}` using `AllocateAndInitializeSid`
3. Call `CreateAppContainerProfile` (no admin needed)
4. Derive the package SID via `DeriveAppContainerSidFromAppContainerName`
5. Build `SECURITY_CAPABILITIES` with package SID + capability SIDs
6. Launch the command via `CreateProcessW` with `EXTENDED_STARTUPINFO_PRESENT` + `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`
7. Wait for exit (`WaitForSingleObject` + `GetExitCodeProcess`)
8. Cleanup: free SIDs + `DeleteAppContainerProfile`

### Well-Known Capability SIDs

All have authority `SECURITY_APP_PACKAGE_AUTHORITY` (value 15) and 2 subauthorities:
- `SECURITY_CAPABILITY_BASE_RID` (3)
- Capability-specific RID (1–12)

| Variant | RID | SID |
|---------|-----|-----|
| `InternetClient` | 1 | `S-1-15-3-1` |
| `InternetClientServer` | 2 | `S-1-15-3-2` |
| `PrivateNetworkClientServer` | 3 | `S-1-15-3-3` |
| `PicturesLibrary` | 4 | `S-1-15-3-4` |
| `VideosLibrary` | 5 | `S-1-15-3-5` |
| `MusicLibrary` | 6 | `S-1-15-3-6` |
| `DocumentsLibrary` | 7 | `S-1-15-3-7` |
| `EnterpriseAuthentication` | 8 | `S-1-15-3-8` |
| `SharedUserCertificates` | 9 | `S-1-15-3-9` |
| `RemovableStorage` | 10 | `S-1-15-3-10` |
| `Appointments` | 11 | `S-1-15-3-11` |
| `Contacts` | 12 | `S-1-15-3-12` |

### Path Grants

On Windows, path grants in profile TOML use `%VAR%` tokens (e.g.
`%SYSTEMROOT%`, `%USERPROFILE%`, `%TEMP%`). The `expand_windows_vars()`
function resolves these at runtime in the backend.

Path grants are **documentary only** — AppContainer doesn't have the same
fine-grained path-level allow/deny model as macOS Seatbelt or Linux Landlock.
Instead, the AppContainer OS policy provides built-in access to:
- `%ProgramFiles%` (read/execute via `ALL RESTRICTED APPLICATION PACKAGES` ACE)
- `%SystemRoot%` (read/execute)
- Package folder at `%LocalAppData%\Packages\<name>\AC` (read/write)

Access to user-installed tools (e.g. `%LocalAppData%\Programs\`) must be
granted by the user moving/reinstalling to Program Files, or by using
`icacls` to add the package SID (discouraged — defeats policy-only approach).

## Cross-Platform Changes

### `profile.rs`
- Added `WindowsCapability` enum
- Added `WindowsExtra` struct
- Added `windows: Option<WindowsExtra>` to `Profile` and `Policy`
- `merge()` unions Windows capabilities across layers

### `filter.rs`
- `merge_windows()` union helper
- `apply_policies()` folds `policy.windows`
- `apply_layer_filter()` clears `windows` on mismatch

### `resolve.rs`
- `is_executable_file()` gated with `#[cfg(unix)]` / `#[cfg(windows)]`
- Windows variant checks `.exe`, `.bat`, `.cmd`, `.ps1` extensions

### `home.rs`
- `real_home()` on Windows tries `USERPROFILE` → `HOME` → `C:\`

### `env.rs`
- Windows allowlist: `HOME`, `PATH`, `USERNAME`, `SYSTEMROOT`, `TMP`, `TEMP`

### `config.rs`
- `builtin_defaults()` → `"windows/system-runtime"`
- Config discovery also checks `%APPDATA%\isol8\`

## Next Steps (Phase 5+)

1. **Tier 2 (Elevated).** Detect non-admin + interactive → `ShellExecuteExW("runas")`
2. **Tier 3 (Job+LowIL).** `CreateJobObject` + `KILL_ON_JOB_CLOSE` + `CreateRestrictedToken` + `SetTokenInformation(TokenIntegrityLevel)`
3. **`--elevate` / `--no-elevate` CLI flags** to control elevation behavior
4. **Resource limits** via Job Object `JOB_OBJECT_LIMIT_*`
5. **Field tests** — real-sandbox scenarios on Windows (like `just field-test` on macOS)
6. **User-specific tool paths** — detect common install locations and suggest migration
