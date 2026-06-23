# Hybrid Sandboxing on Windows: AppContainer + Light User-Mode Hooking

**A Practical Approach for Flexible and Reasonably Secure Process Isolation (2026)**

## 1. Introduction

As of 2026, building custom filesystem policy-based sandboxes on Windows has become more challenging due to stricter kernel driver signing requirements.

The **Hybrid Approach** combines:

- **AppContainer** as a strong, Microsoft-supported isolation foundation (process / IPC / device policy)
- **Light, targeted user-mode DLL hooking** to add per-path **ro/rw** grants that AppContainer cannot express

This model is increasingly used by projects that need more control than pure AppContainer provides, without the complexity and maintenance burden of a full kernel minifilter (like Sandboxie).

### Why This Hybrid Exists

| Approach                    | Security | Flexibility | Ease of Distribution | Maintenance | Recommendation in 2026 |
|----------------------------|----------|-------------|----------------------|-------------|------------------------|
| Pure AppContainer          | Good     | Limited     | Easy                 | Low         | Simple use cases       |
| Pure User-mode Hooking     | Weak     | High        | Easy                 | Medium      | Rarely recommended     |
| **Hybrid (Recommended)**   | **Very Good** | **Good** | **Easy**          | **Medium**  | **Balanced choice**    |
| Full Minifilter (Sandboxie-style) | Excellent | Excellent | Hard              | High        | Maximum security needs |

## 2. Architecture Overview

```
isol8 (parent)
  │
  ├─ merge profile → PathPolicy JSON
  ├─ locate isol8-winhook.dll (beside isol8 binary)
  │
  ├─ [hook DLL present]  HOOK MODE (Tier 1b)
  │     CreateProcessW (CREATE_SUSPENDED, normal user token)
  │     env: ISOL8_PATH_POLICY=<inline JSON>
  │     inject isol8-winhook.dll → LoadLibraryW
  │     resume thread
  │     child: MinHook on CreateFileA/W, FindFirstFileA/W, NtCreateFile
  │
  └─ [no hook DLL]  APPCONTAINER MODE (Tier 1)
        CreateAppContainerProfile + SECURITY_CAPABILITIES
        CreateProcessW (EXTENDED_STARTUPINFO_PRESENT)
        path grants in profile are documentary only
```

### Components (implemented)

| Crate / module | Role |
|----------------|------|
| `crates/isol8-path-policy` | Deny-first path matching + JSON serde (`ISOL8_PATH_POLICY`) |
| `crates/isol8-winhook` | `cdylib` injected into child; hooks file APIs |
| `src/backends/windows_policy.rs` | `Profile` → `PathPolicy` |
| `src/backends/windows_hook.rs` | DLL staging, `LoadLibraryW` inject |
| `src/backends/windows.rs` | Dual launch path (hook vs AppContainer) |

### Policy transport

- **Primary:** `ISOL8_PATH_POLICY` env var with inline JSON (small profiles)
- **Fallback:** `ISOL8_PATH_POLICY_FILE` pointing at a JSON file (large profiles)

### Why hook mode skips AppContainer

AppContainer denies filesystem access before the hook DLL can load. `LoadLibraryW` on a staged DLL fails inside an AppContainer child. Hook mode therefore launches at normal integrity, injects the DLL, then enforces path grants in-process.

AppContainer-only mode remains available when the hook DLL is absent (documentary path grants; env / spawn smoke tests still run).

## 3. Enforcement surface

The hook DLL intercepts:

- `NtCreateFile` (ntdll) — primary path for CRT / cmd / Rust `std::fs`
- `CreateFileA` / `CreateFileW` (kernel32)
- `FindFirstFileA` / `FindFirstFileW` — directory probes

**Not hooked (known gaps):** `NtCreateFile` alternate entry points, memory-mapped I/O, `CopyFile`, registry, network. Determined code can bypass user-mode hooks.

## 4. Build & deploy

```bash
# Build hook DLL (MinGW gcc on PATH — see testing-strategies.md §5.1)
cargo build -p isol8-winhook
cp target/debug/isol8_winhook.dll target/debug/isol8-winhook.dll

# Field tests (hook + probe binary)
just build-windows-test-deps
just field-test-windows
```

Copy `isol8-winhook.dll` next to `isol8.exe` in release installs.

### Release packaging

GitHub Releases (`.github/workflows/release.yml`, tag `v*`) include the hook DLL
in the Windows zip:

| Artifact | Contents |
|----------|----------|
| `windows-x64.zip` | `isol8.exe` + `isol8-winhook.dll` |
| `linux-x64.zip` | `isol8` |
| `macos-arm64.zip` | `isol8` |

The release job installs MinGW (`msys2/setup-msys2`), builds `-p isol8` and
`-p isol8-winhook` for `x86_64-pc-windows-gnu`, and packages via
`_devops/scripts/package-release.sh`. Local equivalent: `just release-windows`.

## 5. Limitations & roadmap

| Item | Status |
|------|--------|
| Per-path ro/rw deny-first | **Enforced** (hook mode) |
| AppContainer + hook simultaneously | **Blocked** by loader policy — deferred |
| Ro read via `GENERIC_READ \| SYNCHRONIZE` | **Enforced** (do not use `FILE_GENERIC_WRITE` in write mask — it shares `SYNCHRONIZE`) |
| Job Object + Low IL (Tier 2) | Planned |
| WFP network tiers | Planned |

## 6. Security notes

- User-mode hooking is **bypassable** by determined code; treat as pragmatic R2 enforcement, not kernel-grade.
- Hook mode trades AppContainer filesystem/IPC isolation for path policy enforcement.
- Keep the hook DLL beside the main binary; tampering the DLL is a local trust-boundary concern (same as the isol8 binary itself).