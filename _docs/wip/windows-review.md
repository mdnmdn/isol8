# Branch review — `features/windows-support`

Reviewed commit `867b056 windows support draft` (vs merge-base `5cafd5c`).
Scope: new Windows AppContainer backend + cross-platform wiring (profile/filter/
home/env/config/resolve) + field-test refactor.

Static review only — `src/backends/windows.rs` is `cfg(windows)`, so none of it
compiles or runs on the macOS review host. Every Windows-specific claim below is
from reading, not execution, and **needs verification on a real Windows box**. The
cross-platform refactors do build and `cargo test` is green on macOS (24 tests).

> **Update (post-review):** blockers #1–#4 below were fixed in later commits on
> `features/windows-support`. See `_docs/windows-support.md` §4 for current status.
> R2 path confinement remains documentary-only.

## Verdict

Direction is reasonable, but this is **not yet enforcing**. The implementation
contradicts its own design doc and likely doesn't produce a real AppContainer, the
env/capability buffers have concrete encoding bugs, and path confinement (R2) is
explicitly not delivered. Treat as an early draft, not a mergeable backend.

## Blockers

### 1. Implementation diverges from the design — and the implemented path is the wrong one

`_docs/wip/windows-support.md` documents the **correct** flow:
`CreateAppContainerProfile` → `DeriveAppContainerSidFromAppContainerName` →
`SECURITY_CAPABILITIES` → `CreateProcessW` with `EXTENDED_STARTUPINFO_PRESENT` +
`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`.

The code does **not** do that. It does:
`OpenProcessToken` → `DuplicateTokenEx` → `SetTokenInformation(TokenAppContainerSid)`
→ `CreateProcessAsUserW` (plain `STARTUPINFOW`, no extended attribute list).

`TokenAppContainerSid` is a *queryable* token property, not a settable one — the
AppContainer (lowbox) SID is established at token-creation time
(`NtCreateLowBoxToken`, or implicitly by the `SECURITY_CAPABILITIES` process
attribute). Setting it via `SetTokenInformation` on an ordinary primary token is
expected to fail (`ERROR_INVALID_PARAMETER`) or to produce a token that is *not* a
lowbox token — i.e. **no AppContainer isolation at all**. The whole
`Win32_Security_Isolation` feature is pulled into `Cargo.toml` but its APIs are
never called.

**Fix:** implement the documented flow. `SECURITY_CAPABILITIES` +
`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` is the only supported,
non-privileged way to launch an AppContainer process.

### 2. Unicode env block passed without `CREATE_UNICODE_ENVIRONMENT` (`windows.rs:174`)

`build_env_block` emits a UTF-16 (`Vec<u16>`) double-null block, but
`CreateProcessAsUserW` is called with `dwCreationFlags = Default::default()` (0).
Without `CREATE_UNICODE_ENVIRONMENT`, Windows reads `lpEnvironment` as ANSI, so the
wide block is garbled and the child gets a broken environment (or fails). Also an
empty `env` yields a single `0`, but an empty block needs **two** nulls.

**Fix:** OR in `CREATE_UNICODE_ENVIRONMENT`; make the empty case `[0, 0]`.

### 3. `TOKEN_GROUPS` capability buffer is misaligned on x64 (`windows.rs:256`, `build_token_groups`)

The buffer places `GroupCount` (4 bytes) immediately followed by the
`SID_AND_ATTRIBUTES` array at offset 4. But `SID_AND_ATTRIBUTES` contains a pointer
(8-byte aligned on x64), so the real `TOKEN_GROUPS.Groups` array starts at offset
**8** (4-byte count + 4-byte padding). Writing the first entry at offset 4 hands
the kernel a misaligned/garbage capability array.

**Fix:** don't hand-pack — use a `#[repr(C)]` struct (or `Vec<SID_AND_ATTRIBUTES>`
with a proper `TOKEN_GROUPS` header) and let the compiler place the array, or
offset entries by `mem::align_of::<SID_AND_ATTRIBUTES>()`.

## Significant

### 4. Command line built by naive `cmd.join(" ")` (`windows.rs:158`)

No quoting/escaping. Any argument containing a space — `C:\Program Files\...`, very
common on Windows — is silently split by `CommandLineToArgvW`. Also enables
argument injection. For a sandbox launching arbitrary agent commands this is a
correctness bug, not an edge case.

**Fix:** apply MSDN `CommandLineToArgvW` quoting per arg (quote on space/tab/quote,
double internal quotes, escape trailing backslashes).

### 5. Path grants are not enforced — R2 is unmet on Windows

By design here, `profile.paths` are "documentary only": AppContainer gives coarse
deny-by-default (user libraries gated behind capabilities) but **no per-path ro/rw
model** like Seatbelt/Landlock. World- and `ALL APPLICATION PACKAGES`-readable
content stays readable; granted paths aren't ACL'd in. So isol8's core promise —
explicit, deny-by-default, per-path grants — is absent on Windows.

Acknowledged in the notes (good), but it's the headline gap: the backend isolates
the *process*, not the *filesystem view*. `render_policy` does label grants
"documentary", but make the "NOT ENFORCED" status loud in `--show-policies` so no
one reads those paths as enforced.

## Minor

- **`InstallationLog.txt` is committed** — an MSYS2 installer log artifact. Remove
  it; add to `.gitignore`.
- **`DuplicateTokenEx` uses `SecurityImpersonation` with `TokenPrimary`** — level
  is ignored for primary tokens; harmless but confusing.
- **Cargo features pulled but unused** (`Win32_Security_Isolation`,
  `Win32_System_JobObjects`, `Win32_UI_Shell`, …) belong with Tiers 2–3; trim until
  those land.
- **Transient package SID** (`hex_token_values`) is fine for uniqueness, but
  unregistered AppContainers lack the named-object boundary profiled ones get —
  note the limitation.

## Cross-platform wiring (profile/filter/home/env/config/resolve)

Compiles and passes on macOS/Linux host tests; shape is right.
`WindowsCapability`/`WindowsExtra` mirror the macOS `capabilities` pattern,
`merge`/`filter` union/clear `windows` symmetrically, `resolve.rs` gates
`is_executable_file` per-OS, `env.rs` adds a Windows allowlist. No regressions
observed. Confirm one thing: `env.rs` allowlists `HOME`, but Windows convention is
`USERPROFILE` — verify the effective-HOME plumbing populates what child tools
expect.

## Recommended order

1. Rewrite the launch path to the documented `SECURITY_CAPABILITIES` flow (#1) —
   nothing else matters without it.
2. Fix env flag (#2) and capability buffer (#3) — they break even a correct flow.
3. Add command-line quoting (#4).
4. Decide and document the R2 story (#5): ACL granted paths to the package SID, or
   state plainly that Windows path confinement is out of scope for Phase 5.
5. Drop `InstallationLog.txt`; add Windows field-test scenarios once #1 works.

---

# Second-pass review — hook mode (commit `cb35a78 windows policy supports with hooks`)

Reviewed `cb35a78` (vs merge-base `cb74cd6`, v0.2.4). Scope: hybrid backend —
`isol8-path-policy` crate, `isol8-winhook` cdylib (MinHook detours), dual launch
(`windows.rs` hook vs AppContainer), `windows_policy.rs`, `windows_hook.rs`, probe
binary, integration tests, release packaging.

Static review again (`cfg(windows)` / cdylib don't build on the macOS host); the
portable `isol8-path-policy` crate does build and test green (5 passing).

## Status of the original blockers

#1–#4 above are **fixed**: real `CreateAppContainerProfile` → `SECURITY_CAPABILITIES`
→ `CreateProcessW(EXTENDED_STARTUPINFO_PRESENT)`; `CREATE_UNICODE_ENVIRONMENT` set
with `[0,0]` empty case; `Vec<SID_AND_ATTRIBUTES>` instead of the hand-packed
`TOKEN_GROUPS`; MSDN per-arg quoting (`quote_arg`). SID lifetimes are correct
(`pkg_sid` freed after `CreateProcessW`). Hook mode (Tier 1b) now delivers real
per-path enforcement, partially addressing #5.

## New blockers (hook mode)

### H1. Path-traversal bypass — `..` / short names / junctions not resolved (`crates/isol8-path-policy/src/lib.rs:94` `normalize_path`)

Matching is string-prefix on a normalized-but-not-canonicalized path. `normalize_path`
swaps slashes, lowercases drive paths, trims trailing `\` — it does **not** collapse
`..`. So with `grant C:\workspace (rw, subpath)`:

```
open C:\workspace\..\Windows\System32\config\SAM  → starts_with "c:\workspace\" → ALLOWED
```

The path resolves outside the grant but passes. Same class: 8.3 short names
(`C:\PROGRA~1\…`) and junctions/symlinks. `..` is trivially exploitable and is **not**
in the documented known-gaps list.

**Fix:** fail-closed — reject any path component equal to `..` (and ideally canonicalize).
Document the short-name/junction limitation alongside mmap/CopyFile.

### H2. Subprocess escape — child processes are not confined

The hook is injected into the launched process only. `CreateProcess` /
`NtCreateUserProcess` is **not** hooked, and the DLL is not propagated to
grandchildren. `ISOL8_PATH_POLICY` is inherited via env, but with no DLL loaded the
grandchild enforces nothing. A confined agent runs `cmd /c type C:\secret` (or spawns
its compiler/shell) and reads anything the user can. For AI agents — which spawn
subprocesses constantly — this removes R2 from essentially every real workload.

**Fix:** hook process creation + auto-inject into children, or state unmissably (docs
+ `--show-policies`) that only the direct child is confined.

### H3. `DELETE` access missing from `WRITE_MASK` (`crates/isol8-winhook/src/lib.rs:197`)

```rust
const WRITE_MASK: u32 = GENERIC_WRITE | 0x0002 | 0x0004 | 0x0008 | 0x0010 | 0x0100;
```

`DELETE` (`0x0001_0000`) is absent, so opening a file with `DELETE` access
(rename/delete, incl. `FILE_FLAG_DELETE_ON_CLOSE`) on an **`ro`** grant is classified
as a read and allowed — an `ro` path can be deleted/renamed.

**Fix:** add `0x0001_0000` to `WRITE_MASK`.

> H2+H3 land in the **default** release config: the zip ships the hook DLL beside the
> binary, so hook mode (FS-hook only, no AppContainer process/IPC/device isolation) is
> the default — the weakest enforcement is the default path. Make this loud in
> `--show-policies`, not just "ENFORCED via hook DLL".

## Significant (hook mode)

### H4. Wrong bit in `WRITE_MASK`: `0x0008` is `FILE_READ_EA` (`isol8-winhook/src/lib.rs:197`)

`0x0008` is a read bit; the intended write bit `FILE_WRITE_EA` (`0x0010`) is already
present. Including `0x0008` makes a read requesting `READ_EA` count as a write →
spuriously denied on `ro` grants. **Fix:** drop `0x0008`.

### H5. Non-drive paths not case-folded (`isol8-path-policy/src/lib.rs:96`)

Lowercasing is gated on `p[1]==':'`. UNC (`\\server\share`) and device paths stay
case-sensitive → case variation bypasses a grant on a case-insensitive FS. **Fix:**
always lowercase, or document drive-paths-only.

### H6. Longest-prefix ranks by raw `rule.path.len()` (`isol8-path-policy/src/lib.rs:85`)

Ranking uses the un-normalized grant length while matching against the normalized
path. A grant written with `/` or a trailing `\` mis-ranks vs a normalized sibling, so
"most specific wins" can pick the wrong rule. **Fix:** rank by
`normalize_path(&rule.path).len()`.

## Minor (hook mode)

- **Suspended child leaked on inject failure** (`windows.rs:164` / `windows_hook.rs`).
  Fail-closed is correct (never runs unconfined), but the child stays `CREATE_SUSPENDED`
  with handles dropped. `TerminateProcess` before returning `Err`.
- **`ISOL8_PATH_POLICY_FILE` fallback defined + documented but unused by the launcher**
  (`windows.rs:124` sets only the inline env var). Wire the file fallback (the DLL reads
  it) or drop the doc claim.
- **Hook mode builds + frees capability SIDs it never uses** (`windows.rs:83` →
  `launch_with_hook` ignores `caps`). Skip `build_capability_sids` when the DLL is present.
- **`GetExitCodeThread` truncates the 64-bit `HMODULE` to 32 bits** (`windows_hook.rs:118`)
  as the `LoadLibraryW` success test; a handle with zero low-32-bits reads as failure.
  Note the heuristic.
- **`fn to_unit(_) -> ()`** (`isol8-winhook/src/lib.rs:190`) may trip
  `clippy::unused_unit`/`let_unit_value`; confirm clippy is clean for `windows-gnu`.
- **`isol8-winhook` is a workspace member**; confirm CI doesn't `cargo build/test
  --workspace` (would try to compile the cdylib on macOS/Linux). `default-members=["."]`
  covers the bare invocation.

## Tests (hook mode)

Good: `path-policy` unit tests (deny-default, ro/rw, explicit-none carve-out, roundtrip)
and `wants_write` tests covering the subtle `GENERIC_READ|SYNCHRONIZE` vs write case.
`windows_spawn.rs` integration tests skip cleanly without the DLL. **Missing** and would
currently fail (the point): `..`-traversal denial (H1), `DELETE`-on-`ro` denial (H3),
subprocess-escape expectation (H2).

## Recommended order (hook mode)

1. H1 (`..` fail-closed) and H3 (`DELETE` in mask) + H4 (drop `0x0008`) — small,
   host-testable in `isol8-path-policy` / `wants_write`. Add the failing unit tests.
2. H2 — hook `CreateProcess*` in the DLL and self-reinject into every child (decided).
3. H5/H6 normalization fixes.
4. Minor cleanups.

## Proposed fixes (hook mode)

Concrete patches for the code-level findings. H2 is a design choice (no snippet — see
options below).

### H1 + H5 + H6 — `normalize_path` and prefix ranking (`crates/isol8-path-policy/src/lib.rs`)

Fail-closed on `..`, always case-fold, and rank by the normalized grant length:

```rust
fn normalize_path(path: &str) -> String {
    // Windows FS is case-insensitive — fold the whole path, not just drive paths (H5).
    let mut p = path.replace('/', "\\").to_ascii_lowercase();
    // Fail-closed on traversal: any ".." component voids the path (H1). Empty → denied.
    if p.split('\\').any(|c| c == "..") {
        return String::new();
    }
    while p.len() > 3 && p.ends_with('\\') {
        p.pop();
    }
    p
}

fn effective_access(&self, path: &str) -> Option<GrantAccess> {
    let mut best: Option<(usize, GrantAccess)> = None;
    for rule in &self.grants {
        if !rule_matches(path, rule) {
            continue;
        }
        let spec = normalize_path(&rule.path).len(); // rank by normalized length (H6)
        if best.map(|(len, _)| spec > len).unwrap_or(true) {
            best = Some((spec, rule.access));
        }
    }
    best.map(|(_, a)| a)
}
```

Add unit tests (host-runnable, currently failing):

```rust
#[test]
fn dotdot_traversal_is_denied() {
    let p = policy(vec![rule(r"C:\workspace", GrantAccess::Rw, GrantMatch::Subpath)]);
    assert!(!p.allows(r"C:\workspace\..\Windows\System32\config\SAM", false));
}

#[test]
fn unc_grant_is_case_insensitive() {
    let p = policy(vec![rule(r"\\srv\Share", GrantAccess::Ro, GrantMatch::Subpath)]);
    assert!(p.allows(r"\\SRV\share\a.txt", false));
}
```

### H3 + H4 — `WRITE_MASK` (`crates/isol8-winhook/src/lib.rs:196`)

Add `DELETE`, drop the stray `FILE_READ_EA` (`0x0008`):

```rust
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_WRITE_DATA: u32 = 0x0002;
const FILE_APPEND_DATA: u32 = 0x0004;
const FILE_WRITE_EA: u32 = 0x0010;       // was joined by stray FILE_READ_EA 0x0008
const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
const DELETE: u32 = 0x0001_0000;         // rename/delete, incl. FILE_FLAG_DELETE_ON_CLOSE
const WRITE_MASK: u32 = GENERIC_WRITE
    | FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_WRITE_EA
    | FILE_WRITE_ATTRIBUTES
    | DELETE;
```

New `wants_write` tests:

```rust
#[test]
fn delete_access_is_write() {
    assert!(wants_write_nt(0x0001_0000, 1));   // DELETE, FILE_OPEN
    assert!(wants_write_win32(0x0001_0000, 3)); // DELETE, OPEN_EXISTING
}

#[test]
fn read_ea_is_not_write() {
    assert!(!wants_write_nt(0x0008, 1));
    assert!(!wants_write_win32(0x0008, 3));
}
```

### H2 — subprocess escape — **decision: hook + reinject**

Resolution: enforce on children. Hook process creation in the child and re-inject the
hook DLL into every descendant so the policy follows the whole process tree (agents
spawn shells/compilers constantly — documenting "direct child only" is not enough).

Implementation:

1. **Hook the create-process path in the DLL.** Detour `CreateProcessW` /
   `CreateProcessA` (and ideally `kernelbase!CreateProcessInternalW`, the common
   sink). In the detour, force `CREATE_SUSPENDED` into `dwCreationFlags`, call the
   original, then inject before resuming.
2. **Reuse the parent's injection logic in-DLL.** The child already has the policy
   (`ISOL8_PATH_POLICY` is inherited via env) and knows its own DLL path
   (`GetModuleFileNameW` on the hook module). Port `inject_dll_and_resume`
   (`VirtualAllocEx` → `WriteProcessMemory` → `CreateRemoteThread(LoadLibraryW)`) into
   the DLL so it self-propagates into each new child, then `ResumeThread` the
   grandchild's primary thread.
3. **Fail-closed.** If injection into a child fails, `TerminateProcess` the child and
   make the create call return failure — never let an un-injected descendant run.
4. **Idempotency.** `DllMain`'s `DLL_PROCESS_ATTACH` already bails if the policy/hooks
   don't install; guard against double-injection (e.g. skip if the hook module is
   already loaded in the target — check via a sentinel env var or module enumeration).

Notes / gaps to track:

- Covers `CreateProcess*`; a child that calls `NtCreateUserProcess` directly (rare
  outside ntdll/CRT) still escapes — document alongside the existing mmap/CopyFile gaps.
- AppContainer mode can't self-inject (LoadLibrary blocked) — propagation applies to
  hook mode only, which is already the enforcing path.
- Add a field-test scenario: confined parent spawns `cmd /c type <denied>` and asserts
  the child is denied.

### Minor — suspended-child leak (`src/backends/windows_hook.rs`)

On any injection failure, terminate the suspended child before returning so it can't
leak as a zombie (it never ran unconfined, so this is cleanup, not a security fix):

```rust
// in inject_dll_and_resume, replace bare `return Err(...)` paths with a helper that
// calls TerminateProcess(process, 1) first, e.g.:
if exit_code == 0 {
    let _ = TerminateProcess(process, 1);
    return Err(Error::Message(format!("LoadLibraryW failed for {}", dll_path.display())));
}
```
