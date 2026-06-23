# isol8 — Windows Backend (Hybrid)

> **Status: Tier 1 + Tier 1b working on real Windows hosts.**
> **AppContainer** (Tier 1): `CreateAppContainerProfile` + `SECURITY_CAPABILITIES` +
> `CreateProcessW` — process/IPC/device isolation; path grants documentary.
> **Hook mode** (Tier 1b): when `isol8-winhook.dll` is beside the binary, path grants
> are **enforced** via user-mode `CreateFile*` / `NtCreateFile` hooks. See
> [`_docs/inbox/windows-policy-approach.md`](inbox/windows-policy-approach.md).

---

## 1. Overview

The Windows backend maps isol8's profile model onto two complementary mechanisms:

| Mode | When | R2 path grants | Process isolation |
|------|------|----------------|-------------------|
| **Hook mode** (Tier 1b) | `isol8-winhook.dll` found beside binary | **Enforced** (deny-first hook) | Normal user token (no AppContainer) |
| **AppContainer** (Tier 1) | Hook DLL absent | Documentary only | AppContainer + capability SIDs |

Hook mode is the default for releases: `windows-x64.zip` ships `isol8.exe` +
`isol8-winhook.dll` together (see §9).

**Primary targets remain macOS and Linux.** Windows R2 enforcement is pragmatic
user-mode hooking — bypassable by determined code, not kernel-grade.

---

## 2. Architecture

### 2.1 Three-tier model (roadmap)

| Tier | Mechanism | Admin required | Status |
|------|-----------|----------------|--------|
| 1 | AppContainer + `SECURITY_CAPABILITIES` | No | Implemented |
| 1b | User-mode hook DLL (`isol8-winhook`) | No | Implemented |
| 2 | Elevated AppContainer (`ShellExecuteExW("runas")`) | Yes | Planned |
| 3 | Job Object + Low IL + Restricted Token | No | Planned |

### 2.2 Profile model mapping

- **Deny-by-default path grants (R2)** — hook mode serializes merged grants to JSON
  (`ISOL8_PATH_POLICY` env var) and enforces in the child via MinHook detours.
- **`[windows].capabilities`** — twelve `WindowsCapability` variants → `S-1-15-3-{N}`
  SIDs passed in `SECURITY_CAPABILITIES` (AppContainer mode only).
- **`paths`** — expanded via `expand_windows_vars()` (`%SYSTEMROOT%`, `%TEMP%`, …);
  converted to `PathPolicy` JSON by `windows_policy.rs`.
- **HOME** — `env.rs` sets `HOME` first; `USERPROFILE`, `APPDATA`, etc. follow the
  effective scratch home.

### 2.3 Hook-mode launch flow

1. Merge profile → `PathPolicy` JSON.
2. `CreateProcessW` with `CREATE_SUSPENDED` and inline `ISOL8_PATH_POLICY` env.
3. Remote `LoadLibraryW` inject `isol8-winhook.dll`.
4. Resume thread; child hooks file APIs before main runs.

AppContainer is skipped in hook mode because it blocks `LoadLibraryW` before the
hook can arm (see approach doc §3).

### 2.4 AppContainer launch flow (fallback)

1. `CreateAppContainerProfile` / `DeriveAppContainerSidFromAppContainerName`.
2. `SECURITY_CAPABILITIES` + `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`.
3. `CreateProcessW` with `EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT`.
4. Non-blocking `SandboxChild`; profile deleted on `wait`/`kill`.

---

## 3. Components

| Path | Role |
|------|------|
| `src/backends/windows.rs` | Dual launch: hook mode vs AppContainer |
| `src/backends/windows_hook.rs` | DLL discovery, `LoadLibraryW` inject |
| `src/backends/windows_policy.rs` | `Profile` → `PathPolicy` JSON |
| `crates/isol8-path-policy` | Deny-first path matching (shared with hook DLL) |
| `crates/isol8-winhook` | `cdylib`: MinHook on `CreateFileA/W`, `FindFirstFileA/W`, `NtCreateFile` |
| `src/bin/isol8-probe.rs` | Minimal read/write probe for field tests |
| `profiles/windows/system-runtime.toml` | System paths + `internet-client` capability |

---

## 4. Capability SIDs (AppContainer mode)

| `WindowsCapability` variant | RID | SID |
|-----------------------------|-----|-----|
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

---

## 5. Path confinement (R2)

### Hook mode — enforced

When `isol8-winhook.dll` is present:

- Profile path grants are serialized to JSON and passed via `ISOL8_PATH_POLICY`.
- The hook DLL denies-by-default and allows only matching grants (longest-prefix wins).
- `ro` grants allow read opens; `rw` allows read and write.
- Known gaps: memory-mapped I/O, `CopyFile`, registry, unhooked syscalls — see approach doc.

### AppContainer mode — documentary

Without the hook DLL, `render_policy` labels path grants as **documentary**. The
AppContainer model does not provide per-path ro/rw APIs like Seatbelt or Landlock.

---

## 6. Limitations vs. macOS and Linux

| Property | macOS (Seatbelt) | Linux (Landlock) | Windows (hook mode) |
|----------|-----------------|-----------------|---------------------|
| Per-path ro/rw control | Yes | Yes | Yes (user-mode hook) |
| Deny-by-default fs | Yes | Yes | Yes (hook mode) |
| Bypass resistance | Strong | Strong | Weak (hook bypassable) |
| Process confinement | Yes | Yes (no-new-privs) | Partial (no AppContainer in hook mode) |
| Network isolation | Via capabilities | Via netns (Phase 3) | Not yet (Phase 3 / WFP) |
| HOME replacement | Full | Full (bind mount) | Best-effort (env vars) |
| No-admin required | Yes | Yes | Yes |

---

## 7. Roadmap

**Done (Tier 1 / 1b):**

- AppContainer launch path (review blockers fixed)
- Hook DLL path enforcement
- Field tests scenarios 01–07 (with hook DLL)
- GitHub Release packages `isol8.exe` + `isol8-winhook.dll`

**Planned (Phase 5+):**

- Simultaneous AppContainer + hook (blocked by loader policy today)
- Tier 2 elevated retry (`--elevate` / `--no-elevate`)
- Tier 3 Job Object + Low IL
- WFP network tiers
- Additional hook surfaces (`CopyFile`, mmap, …)

---

## 8. Building, testing, and release

### Local dev

```powershell
# Requires MinGW gcc on PATH (see testing-strategies.md §5.1)
cargo build
cargo build -p isol8-winhook
copy target\debug\isol8_winhook.dll target\debug\isol8-winhook.dll

cargo test
cargo run --bin isol8-field-test
# or:
just field-test-windows
```

### Release

```sh
just release-windows    # isol8.exe + isol8-winhook.dll in target/release/
```

GitHub Releases (tag `v*`) build via `.github/workflows/release.yml`:

- **windows-x64.zip** — `isol8.exe` + `isol8-winhook.dll`
- **linux-x64.zip** / **macos-arm64.zip** — `isol8` binary only

Packaging script: `_devops/scripts/package-release.sh`.

### Introspection

```sh
isol8 --show-policies cmd /c echo hi
isol8 --dry-run echo hi
```

Hook mode reports path grants as **ENFORCED via hook DLL**; AppContainer-only mode
reports them as **DOCUMENTARY**.

---

## 9. Related docs

| Doc | Contents |
|-----|----------|
| [`_docs/inbox/windows-policy-approach.md`](inbox/windows-policy-approach.md) | Hybrid design, build, security notes |
| [`_docs/testing-strategies.md`](testing-strategies.md) | Field tests, Windows prerequisites, release zips |
| [`_docs/wip/windows-review.md`](wip/windows-review.md) | Historical code review (AppContainer blockers) |
| [`AGENTS.md`](../AGENTS.md) | Contributor guide |