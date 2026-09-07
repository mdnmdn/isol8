# isol8 — Windows Backend (Hook DLL)

> **Status: working on real Windows hosts.**
> Path grants are **enforced** via an embedded `isol8-winhook.dll` payload:
> `isol8-core`'s `build.rs` builds the cdylib and embeds it with `include_bytes!`;
> at runtime the engine materializes it to `%LOCALAPPDATA%\isol8\` and injects it
> into each confined child. This works for both the `isol8` CLI and library
> embedders (`isol8` / `isol8-core`).

---

## 1. Overview

The Windows backend maps isol8's profile model onto user-mode file API hooking:

| Property | Behavior |
|----------|----------|
| **R2 path grants** | **Enforced** (deny-first `CreateFile*` / `NtCreateFile` hooks) |
| **Process isolation** | Normal user token + suspended `CreateProcessW` + DLL inject |
| **Library embedding** | Hook DLL ships inside `isol8-core` — no separate artifact required |

**Primary targets remain macOS and Linux.** Windows R2 enforcement is pragmatic
user-mode hooking — bypassable by determined code, not kernel-grade.

AppContainer was removed: without the hook DLL it could not enforce per-path `ro`/`rw`,
so it added complexity without matching isol8's core contract.

---

## 2. Architecture

### 2.1 Profile model mapping

- **Deny-by-default path grants (R2)** — merged grants serialize to JSON
  (`ISOL8_PATH_POLICY` env var) and are enforced in the child via MinHook detours.
- **`paths`** — expanded via `expand_windows_vars()` (`%SYSTEMROOT%`, `%TEMP%`, …);
  converted to `PathPolicy` JSON by `windows_policy.rs`.
- **`[windows].capabilities`** — parsed for forward compatibility (network tiers);
  not applied at spawn today.
- **HOME** — `env.rs` sets `HOME` first; `USERPROFILE`, `APPDATA`, etc. follow the
  effective scratch home.

### 2.2 Launch flow

1. Merge profile → `PathPolicy` JSON.
2. Resolve hook DLL: `Spec.win_hook_dll` → `ISOL8_WINHOOK_DLL` → sidecar beside the
   host binary (dev override) → embedded payload.
3. `CreateProcessW` with `CREATE_SUSPENDED` and inline `ISOL8_PATH_POLICY` env.
4. Remote `LoadLibraryW` inject `isol8-winhook.dll`.
5. Resume thread; child hooks file APIs before main runs.
6. **Child propagation:** the hook DLL detours `kernelbase!CreateProcessInternalW`
   and `CreateProcessA`. Every descendant is created suspended, receives the same hook
   DLL via remote `LoadLibraryW`, then resumes. Policy travels via inherited
   `ISOL8_PATH_POLICY` env. Injection failure is fail-closed.

### 2.3 Embedded DLL payload (CLI and embedders)

`crates/isol8-core/build.rs` (Windows targets only):

1. Builds `isol8-winhook` for the active `TARGET`/`PROFILE` (or honors
   `ISOL8_WINHOOK_DLL` at **build** time).
2. Copies `isol8_winhook.dll` into `OUT_DIR` and generates `winhook_embedded.rs`.
3. `windows_hook.rs` materializes bytes to
   `%LOCALAPPDATA%\isol8\isol8-winhook-<hash>.dll` on first use (content-addressed cache).

Consumers depending on `isol8` or `isol8-core` get path enforcement without copying a
DLL beside their binary. Local hook development can still override by placing
`isol8_winhook.dll` next to the host binary, setting `Spec.win_hook_dll`, or exporting
`ISOL8_WINHOOK_DLL` at runtime.

Missing required DLL is **fail-closed** — no silent downgrade to an unconfined run.

---

## 3. Components

| Path | Role |
|------|------|
| `crates/isol8-core/build.rs` | Embed profiles, recipes, and winhook DLL bytes (Windows) |
| `crates/isol8-core/src/backends/windows.rs` | Hook-mode `CreateProcessW` launch |
| `crates/isol8-core/src/backends/windows_hook.rs` | DLL resolve/materialize + `LoadLibraryW` inject |
| `crates/isol8-core/src/backends/windows_policy.rs` | `Profile` → `PathPolicy` JSON |
| `crates/isol8-path-policy` | Deny-first path matching (shared with hook DLL) |
| `crates/isol8-winhook` | `cdylib`: MinHook on `CreateFileA/W`, `FindFirstFileA/W`, `NtCreateFile` |
| `crates/isol8-cli/src/bin/isol8-probe.rs` | Minimal read/write probe for field tests |
| `profiles/windows/system-runtime.toml` | System paths for Windows agents |

---

## 4. Path confinement (R2)

- Profile path grants serialize to JSON and pass via `ISOL8_PATH_POLICY`.
- The hook DLL denies-by-default and allows only matching grants (longest-prefix wins).
- `ro` grants allow read opens; `rw` allows read and write.
- Known gaps: 8.3 short names, junctions/symlinks, memory-mapped I/O, `CopyFile`,
  registry, direct `NtCreateUserProcess` (bypassing hooked `CreateProcess*`).
  `..` path components are fail-closed (denied).

---

## 5. Limitations vs. macOS and Linux

| Property | macOS (Seatbelt) | Linux (Landlock) | Windows (hook) |
|----------|-----------------|-----------------|----------------|
| Per-path ro/rw control | Yes | Yes | Yes (user-mode hook) |
| Deny-by-default fs | Yes | Yes | Yes |
| Bypass resistance | Strong | Strong | Weak (hook bypassable) |
| Process confinement | Yes | Yes (no-new-privs) | Partial (normal user token) |
| Network isolation | Via capabilities | Via netns (Phase 3) | Not yet (Phase 3 / WFP) |
| HOME replacement | Full | Full (bind mount) | Best-effort (env vars) |
| No-admin required | Yes | Yes | Yes |

---

## 6. Embedding

```rust
let code = isol8::Sandbox::new()
    .profile("base")
    .profile("windows/system-runtime")
    .grant_rw(r"C:\my\project")
    .run(["cmd", "/c", "echo hi"])?;
```

No extra feature flag. `default-features = false` still embeds the hook DLL because
the payload lives in `isol8-core`, which the facade always depends on.

Optional override for hosts that ship their own copy:

```rust
let mut spec = isol8::Spec::new(["cmd", "/c", "dir"]);
spec.win_hook_dll = Some(r"C:\opt\isol8-winhook.dll".into());
```

---

## 7. Roadmap

**Done:**

- Hook DLL path enforcement
- Embedded DLL payload for CLI and library consumers
- Field tests scenarios 01–07 and 09 hook spawn
- GitHub Release packages `isol8.exe` (DLL embedded)

**Planned (Phase 5+):**

- Job Object + Low IL + Restricted Token
- WFP network tiers
- Registry hooks

---

## 8. Build

```bash
# Windows: build isol8 (isol8-core build.rs builds + embeds isol8-winhook automatically)
cargo build

# Optional: iterate on the hook crate alone
cargo build -p isol8-winhook

# Field tests (probe binary)
just field-test-windows

# Override embedded DLL at build time
ISOL8_WINHOOK_DLL=/path/to/isol8_winhook.dll cargo build
```

Requires a Windows C toolchain (MSVC or MinGW-w64 `gcc`) so `isol8-winhook` can
compile. See `_docs/testing-strategies.md`.

Home `link` materialization needs symlink privilege — enable **Developer Mode**
or run elevated; without it link tests skip (unit) / report `SKIP` (field
scenarios 18–19).

### Release packaging

| Artifact | Contents |
|----------|----------|
| `windows-x64.zip` | `isol8.exe` (hook DLL embedded) |
| `linux-x64.zip` | `isol8` |
| `macos-arm64.zip` | `isol8` |

---

## 9. Dry-run output

`--show-policies` / `DryRun` report path grants as **ENFORCED via hook DLL**
and show the materialized DLL path when resolved.

---

## Related docs

| Doc | Contents |
|-----|----------|
| [`embedding.md`](embedding.md) §7 | Embedder contract for the hook payload |
| [`inbox/windows-policy-approach.md`](inbox/windows-policy-approach.md) | Historical hybrid design notes |
| [`testing-strategies.md`](testing-strategies.md) | Field test matrix |
| [`profile-model.md`](profile-model.md) | Profile TOML, `[[policies]]`, merge rules |
