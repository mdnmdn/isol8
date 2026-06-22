# isol8 — Windows AppContainer Backend

> **Status: Tier 1 implemented (review feedback addressed).**
> The backend now uses the documented `CreateAppContainerProfile` + `Derive...` +
> `SECURITY_CAPABILITIES` + `CreateProcessW(EXTENDED_STARTUPINFO_PRESENT | ...)` path.
> Env block, command quoting, and attr list alignment fixed. Path grants remain
> documentary (inherent to AppContainer model). Full enforcement verified on a
> real Windows host is still recommended before production use.

---

## 1. Overview

The Windows backend is the Phase 5 target for `isol8`. The goal is to provide
AppContainer-based process isolation on Windows, mapping isol8's profile model
(deny-by-default, composable path grants, Windows capability SIDs) onto the
`SECURITY_CAPABILITIES` / `CreateProcessW` launch path that Windows offers for
unprivileged AppContainer creation.

This document covers:

- What the backend currently does (and does not do).
- What the correct, intended implementation looks like.
- The concrete blockers identified by code review that must be fixed before the
  backend can enforce anything.
- The limitations inherent to the AppContainer model vs. macOS Seatbelt and Linux Landlock.
- The Phase 5 roadmap.

**Primary targets remain macOS and Linux.** The Windows backend is provided for
completeness and future development; it is not part of the Phase 1 MVP.

---

## 2. Intended architecture

### 2.1 Three-tier model

The Windows backend is designed around three escalating tiers of isolation. Only Tier 1
is in scope for Phase 5 MVP; Tiers 2–3 are deferred.

| Tier | Mechanism | Admin required | Intended enforcement |
|------|-----------|----------------|----------------------|
| 1 | AppContainer + `SECURITY_CAPABILITIES` | No | Deny-by-default FS, IPC, device isolation |
| 2 | Elevated AppContainer (`ShellExecuteExW("runas")`) | Yes | Tier 1, retried via UAC when needed |
| 3 | Job Object + Low IL + Restricted Token | No | Process tree teardown, write-restriction, privilege reduction |

### 2.2 Profile model mapping

isol8's profile model maps onto Windows concepts as follows:

- **Deny-by-default process confinement** — AppContainer provides a low-privilege
  token that loses access to most user-level resources (named pipes, COM objects,
  most registry hives) unless explicitly granted.
- **`[windows].capabilities`** — each `WindowsCapability` variant maps to a
  well-known capability SID (`S-1-15-3-{N}`) in the
  `SECURITY_APP_PACKAGE_AUTHORITY` authority (value 15). The capability SID list
  is passed as `SECURITY_CAPABILITIES.Capabilities` to the kernel when launching
  the process.
- **`paths`** — profile path grants on Windows are documentary placeholders. The
  AppContainer model does not provide the same per-path ro/rw API as Seatbelt
  or Landlock; see section 5 for the full limitation.
- **`%VAR%` tokens** — Windows-specific path grants use `%SYSTEMROOT%`,
  `%TEMP%`, etc., expanded at runtime by `expand_windows_vars()`.

### 2.3 Intended launch flow (the correct design)

The `_docs/wip/windows-support.md` design document specifies this flow:

1. Generate a unique container name (e.g. `Isol8.<hex>`).
2. Allocate capability SIDs (`S-1-15-3-{N}`) via `AllocateAndInitializeSid`.
3. Call `CreateAppContainerProfile` to register the named container (no admin needed).
4. Derive the package SID via `DeriveAppContainerSidFromAppContainerName`.
5. Build a `SECURITY_CAPABILITIES` struct with the package SID and capability SIDs.
6. Launch the command via `CreateProcessW` with `EXTENDED_STARTUPINFO_PRESENT` and
   a process attribute list containing `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`.
7. Wait for exit (`WaitForSingleObject` + `GetExitCodeProcess`).
8. Clean up: free SIDs, call `DeleteAppContainerProfile`.

This is the only supported, non-privileged API surface for launching a real
AppContainer (lowbox) process. The package SID established in step 4 is what
Windows uses to scope named objects, ACLs, and capability checks.

---

## 3. Current implementation

### 3.1 What `src/backends/windows.rs` does

The backend follows the documented Tier 1 flow:

1. `CreateAppContainerProfile` — registers a unique `Isol8.<hex>` container (no admin).
2. On `ERROR_ALREADY_EXISTS`, `DeriveAppContainerSidFromAppContainerName` reuses the SID.
3. `SECURITY_CAPABILITIES` + `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` on a
   `STARTUPINFOEXW` attribute list.
4. `CreateProcessW` with `EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT`,
   an explicit UTF-16 env block, MSDN-compatible command-line quoting, and
   `lpApplicationName` when `argv[0]` is absolute.
5. Non-blocking `SandboxChild::windows` handle; `wait`/`kill` delete the profile.

Review blockers #1–#4 from `_docs/wip/windows-review.md` are addressed in the
current tree. **Full enforcement on a real Windows host is still recommended**
before production use (run `cargo run --bin isol8-field-test`).

### 3.2 Capability SIDs (12 supported)

The RID mapping in `CAPABILITY_RIDS` is correct and complete for the twelve
well-known capability SIDs. These will be usable once the launch path is rewritten.

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

### 3.3 `%VAR%` path expansion

`home::expand_windows_vars()` (called from `expand_grant` during merge) expands
twelve well-known `%VAR%` tokens against the host environment at runtime:

```
%SYSTEMROOT%    %USERPROFILE%   %LOCALAPPDATA%  %APPDATA%
%PROGRAMFILES%  %PROGRAMFILES(X86)%  %ALLUSERSPROFILE%
%SYSTEMDRIVE%   %TEMP%          %TMP%
%HOMEDRIVE%     %HOMEPATH%
```

This expansion is called in `render_policy` for display, and must also be called
in any enforcement path once path grants become real.

### 3.4 System profile (`windows/system-runtime`)

`profiles/windows/system-runtime.toml` declares a system profile that requires
`base` and grants read-only access to `%SYSTEMROOT%`, `%PROGRAMFILES%`,
`%PROGRAMFILES(X86)%`, `%ALLUSERSPROFILE%`, `%SYSTEMDRIVE%`, and read-write
access to `%TEMP%` and `%TMP%`. It also requests the `internet-client` capability.
Like all path grants on Windows today, these are **documentary only** (see section 5).

### 3.5 `render_policy`

`WindowsBackend::render_policy` prints the capability list and path grants,
explicitly labelling the grants as "documentary". This text is what `--show-policies`
and `--dry-run` display on Windows. The label is intentional and must remain.

### 3.6 `SandboxChild` (non-blocking)

`Backend::spawn` returns a live `SandboxChild::windows` handle immediately.
`wait()` blocks on `WaitForSingleObject`, reads the exit code, closes the process
handle, and best-effort calls `DeleteAppContainerProfile` for the invocation's
container name.

---

## 4. Resolved review items (commit `867b056` → current)

The static review in `_docs/wip/windows-review.md` identified four implementation
bugs in the original draft. Status in the current tree:

| Item | Status |
|------|--------|
| Wrong launch path (`SetTokenInformation` / `CreateProcessAsUserW`) | **Fixed** — `CreateAppContainerProfile` + `SECURITY_CAPABILITIES` + `CreateProcessW` |
| Missing `CREATE_UNICODE_ENVIRONMENT` / empty env block | **Fixed** |
| Misaligned `TOKEN_GROUPS` hand-pack | **Fixed** — `Vec<SID_AND_ATTRIBUTES>` passed to `SECURITY_CAPABILITIES` |
| Naive `cmd.join(" ")` quoting | **Fixed** — `quote_arg` / `build_quoted_command_line` |

---

## 5. PATH CONFINEMENT IS NOT ENFORCED ON WINDOWS

> **R2 (per-path ro/rw control) is not met on Windows.** This is the headline
> limitation of the AppContainer model and must be clearly communicated to users.

Profile `paths` entries are **documentary only** on Windows. `render_policy`
labels them as such. The AppContainer model provides coarse deny-by-default
isolation (UWP-style: named pipes, COM, registry, device access are blocked) but
does **not** provide a per-path filesystem allow/deny API analogous to macOS
Seatbelt's `(allow file-read* (subpath …))` or Linux Landlock's `PathBeneath`
ruleset.

What AppContainer does control by default:

- `%ProgramFiles%` and `%SystemRoot%` are readable via the
  `ALL RESTRICTED APPLICATION PACKAGES` ACE on those directories.
- The package's own data folder (`%LocalAppData%\Packages\<name>\AC`) is
  readable and writable.
- Everything else (user profile, documents, drives) is inaccessible by default
  unless the user adds an `icacls` ACE for the package SID — which requires
  knowing the SID ahead of time and modifies the host filesystem, defeating
  the policy-only approach.

**Consequence:** on Windows, `isol8` confines the *process* (network, IPC, device
access) but does not confine the *filesystem view*. A confined process can still
read and write any path the host user can access. The `--show-policies` and
`--dry-run` output on Windows must make this clear.

Possible future paths for R2 on Windows:

1. Grant the package SID explicit ACEs on profile-declared paths at launch time
   (using `SetNamedSecurityInfo` or similar), then revoke on exit. Invasive:
   modifies the host ACL.
2. Declare Windows path confinement out of scope for Phase 5 and document the
   limitation explicitly. Isolation remains at the process/network/IPC level.

The decision on which path to take is deferred to Phase 5 planning.

---

## 6. Limitations vs. macOS and Linux

| Property | macOS (Seatbelt) | Linux (Landlock) | Windows (AppContainer) |
|----------|-----------------|-----------------|------------------------|
| Per-path ro/rw control | Yes (`subpath`, `literal`) | Yes (`PathBeneath`) | No — documentary only |
| Deny-by-default fs | Yes | Yes | Partial (UWP objects only) |
| Process confinement | Yes | Yes (no-new-privs) | Intended (not yet working) |
| Network isolation | Via profile capabilities | Via netns (Phase 3) | Via capability SIDs (not yet) |
| HOME replacement | Full (Seatbelt allows rebinding) | Full (bind mount) | Best-effort (`USERPROFILE` vs `HOME`) |
| Ancestor metadata | Emitted in SBPL | Not needed (Landlock handles subtrees) | Not applicable |
| No-admin required | Yes | Yes | Yes (Tier 1) |
| Verified enforcing | Yes | Yes (WSL2 kernel 5.15) | No — not yet |

### HOME convention

Unix tools running on Windows under WSL or native Cygwin/MSYS2 environments
expect `HOME`; native Win32 tools expect `USERPROFILE`. The `env.rs` allowlist
includes `HOME`, but `home::real_home()` on Windows tries `USERPROFILE` → `HOME`
→ `C:\` as the fallback chain. Verify that tools invoked under isol8 receive
whichever variable they expect; this may require passing both through the env
sanitization step.

### Transient vs. named package SID

The current code generates a unique transient SID from `{pid, nanos, counter}`.
Named AppContainers (via `CreateAppContainerProfile`) get a stable, registered SID
associated with the container name, which gives them access to the named-object
namespace boundary that profiled AppContainers receive. Transient SIDs lack this
boundary; note this limitation in documentation and consider whether it matters for
the agent use case.

---

## 7. Roadmap (Phase 5)

The following work items are needed to bring the Windows backend to enforcing status.
Items are listed in dependency order.

1. **Rewrite launch to `SECURITY_CAPABILITIES` flow (BLOCKER 1).**
   Replace the `OpenProcessToken` / `DuplicateTokenEx` / `SetTokenInformation`
   approach with the documented `CreateAppContainerProfile` →
   `DeriveAppContainerSidFromAppContainerName` → `SECURITY_CAPABILITIES` →
   `CreateProcessW` + `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` flow. Nothing
   else matters until this is correct.

2. **Fix env block (`CREATE_UNICODE_ENVIRONMENT`) and empty-map case (BLOCKER 2).**

3. **Fix `TOKEN_GROUPS` alignment (BLOCKER 3).** Use a proper `#[repr(C)]`
   layout or explicit alignment offset for the capabilities buffer.

4. **Add command-line quoting.** Implement MSDN-compatible per-argument quoting
   before concatenating the command line.

5. **Decide the R2 story.** Either implement ACL-based path grants against the
   package SID (invasive, modifies host ACLs), or formally document that
   filesystem path confinement is out of scope on Windows for Phase 5. Update
   `--show-policies` output to state this unambiguously, not just as a label.

6. **Make `SandboxChild` non-blocking.** The current implementation blocks
   synchronously. Return a real non-blocking handle so the `Backend::spawn`
   contract is honoured on Windows.

7. **Trim unused Cargo features.** `Win32_Security_Isolation`,
   `Win32_System_JobObjects`, `Win32_UI_Shell` etc. belong with Tiers 2–3.
   Remove them until those tiers land.

8. **Add Windows field-test scenarios.** Mirror the `just field-test` suite
   (scenarios 1–9 cross-platform) for Windows once Tier 1 enforces correctly.
   Verify on a real Windows host; the macOS/Linux CI host cannot compile or
   execute `cfg(windows)` code.

9. **Tier 2 — Elevated retry (Phase 5+).** Detect non-admin + interactive
   context; retry via `ShellExecuteExW("runas")` for cases requiring UAC.
   Controlled by `--elevate` / `--no-elevate` CLI flags.

10. **Tier 3 — Job Object + Low IL (Phase 5+).** `CreateJobObject` +
    `KILL_ON_JOB_CLOSE` + `CreateRestrictedToken` +
    `SetTokenInformation(TokenIntegrityLevel)` for process tree teardown and
    write-restriction. Resource limits via `JOB_OBJECT_LIMIT_*`. WFP for
    network enforcement.

---

## 8. Files

| Path | Role |
|------|------|
| `src/backends/windows.rs` | Backend implementation (current draft) |
| `profiles/windows/system-runtime.toml` | System profile (`%SYSTEMROOT%`, `%TEMP%`, etc.) |
| `_docs/wip/windows-support.md` | Original design doc (intended correct flow) |
| `_docs/wip/windows-review.md` | Code review: concrete blockers and gaps |
| `AGENTS.md` | Windows backend bullet and Phase 5 roadmap entry |
| `_docs/project-structure.md` | Module blueprint (§3 `backends/windows.rs` entry) |

---

## 9. Building and testing

```sh
# Cross-compile from macOS/Linux (requires the MSVC target):
cargo build --target x86_64-pc-windows-msvc

# The Windows backend is cfg(windows)-gated and does not compile natively
# on macOS/Linux. Cross-compilation verifies syntax and type-checking only
# — it cannot run or enforce.

# On a real Windows host:
cargo build
cargo test
isol8 --show-policies cmd /c echo hi   # inspect the documentary policy output
```

Run `cargo run --bin isol8-field-test` on a Windows host to verify AppContainer
spawn and env scenarios. Path scenarios remain skipped (R2 documentary only).
