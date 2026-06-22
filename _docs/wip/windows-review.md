# Branch review — `features/windows-support`

Reviewed commit `867b056 windows support draft` (vs merge-base `5cafd5c`).
Scope: new Windows AppContainer backend + cross-platform wiring (profile/filter/
home/env/config/resolve) + field-test refactor.

Static review only — `src/backends/windows.rs` is `cfg(windows)`, so none of it
compiles or runs on the macOS review host. Every Windows-specific claim below is
from reading, not execution, and **needs verification on a real Windows box**. The
cross-platform refactors do build and `cargo test` is green on macOS (24 tests).

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
