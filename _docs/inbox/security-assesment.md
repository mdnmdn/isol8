# isol8 Security Assessment

**Date:** 2026-06-24  
**Last updated:** 2026-06-24 (C1, C2, C3 fixed)  
**Scope:** Full codebase — profile/policy engine, env/home isolation, Linux Landlock backend, macOS Seatbelt backend, Windows hook/AppContainer backend, CLI surface  
**Method:** Static analysis across four parallel review tracks (core policy, Linux backend, macOS/Windows backend, CLI threat model)  
**Branch:** `claude/clippy-windows-pr-fixes-8g80p8`

---

## Executive Summary

isol8 is a deny-by-default sandbox whose core invariants are sound: the profile merge correctly implements deny-first precedence, TOML structures use `deny_unknown_fields` throughout, and the environment allowlist is conservative. The macOS and Linux backends enforce policy at the OS level with no obvious bypass of the main execution path.

Three **Critical** issues were identified and have since been fixed (see C1–C3 below). Multiple **High** issues remain open and require attention before this tool can be considered production-grade for security-sensitive deployments:

1. ~~On **Linux**, an empty profile silently bypasses Landlock — the process runs with `PR_SET_NO_NEW_PRIVS` but no filesystem restriction.~~ **Fixed.**
2. ~~On **Windows**, the hook DLL search path includes the current working directory, enabling trivial DLL hijacking.~~ **Fixed.**
3. ~~In the **profile/home** engine, seed entries accept `..` path components that can traverse outside the home boundary.~~ **Fixed.**

A summary risk table is at the end of this document.

---

## Findings

### Critical

---

#### C1 — Empty Landlock Ruleset Silently Bypasses Enforcement ✅ Fixed
**File:** `src/backends/linux.rs:237–238`  
**CWE:** CWE-284 (Improper Access Control)  
**Status:** Fixed in `claude/clippy-windows-pr-fixes-8g80p8`

When `rules.is_empty()`, `apply_landlock()` returned `Ok(())` without calling `restrict_self()`. The child process executed with `PR_SET_NO_NEW_PRIVS` set but zero Landlock rules — full filesystem access. A profile with no path grants appeared to run inside a sandbox but did not.

**Impact:** Complete bypass of Linux filesystem confinement for any profile that resolves to zero path grants.

**Fix applied:** Removed the early-return guard. A ruleset with `handled_accesses` but no `PathBeneath` rules enforces deny-all for those access types — exactly the correct behaviour. `restrict_self()` is now always called when Landlock is available.

---

#### C2 — Windows Hook DLL Hijacking via CWD Search ✅ Fixed
**File:** `src/backends/windows_hook.rs:37, 44–48`  
**CWE:** CWE-427 (Uncontrolled Search Path Element)  
**Status:** Fixed in `claude/clippy-windows-pr-fixes-8g80p8`

The DLL resolution fell back to `PathBuf::from(HOOK_DLL_NAME)` — a bare filename with no path component — causing Windows to search the current working directory first. An attacker who placed a malicious `isol8-winhook.dll` in any directory the user navigated to would have it injected into every sandboxed child, gaining full control of the hook layer.

**Impact:** Complete sandbox bypass on Windows; the attacker's DLL replaces the path policy with permissive grants and can exfiltrate data or escalate privileges.

**Fix applied:** The bare-filename `paths.push(PathBuf::from(HOOK_DLL_NAME))` fallback was removed from `hook_dll_search_paths()`. The search is now restricted exclusively to the binary's own directory tree (up to 3 parent levels). Note: Authenticode verification (H2) remains open as a defence-in-depth measure.

---

#### C3 — Seed Path `..` Traversal Outside Home Boundary ✅ Fixed
**File:** `src/home.rs:191–207`  
**CWE:** CWE-22 (Path Traversal)  
**Status:** Fixed in `claude/clippy-windows-pr-fixes-8g80p8`

The `seed()` function expanded seed entries against the real home and copied them into the scratch home without validating that the path stayed within the home tree. A profile with `seed = ["~/../../../etc/passwd"]` caused `copy_readonly()` to read a file outside `$HOME`.

Additionally, `copy_readonly()` only checked for symlinks at the top level. During recursive directory traversal, nested symlinks (e.g. `~/.gitconfig -> /etc/shadow`) were followed by `std::fs::copy`, leaking files from outside the home.

**Impact:** A crafted or attacker-controlled profile could exfiltrate arbitrary host files into the confined environment, violating deny-by-default.

**Fix applied (two-part):**

1. `seed()` now rejects any entry that contains `..` path components or is an absolute path before joining with the real home:
```rust
if Path::new(rel).is_absolute() || rel.split('/').any(|c| c == "..") {
    return Err(Error::Profile(format!("seed entry escapes home boundary: {entry}")));
}
```

2. `copy_readonly()` now checks `meta.file_type().is_symlink()` at every level of recursion and silently skips symlinks, preventing any follow-through to targets outside the home.

Two regression tests were added: `seed_rejects_dotdot_traversal` and `seed_skips_symlinks`.

---

### High

---

#### H1 — Raw SBPL Passthrough Bypasses `(deny default)`
**File:** `src/backends/macos.rs:212–219`  
**CWE:** CWE-94 (Code Injection)

Profile layers can include a `raw` field containing arbitrary Seatbelt SBPL text that is appended verbatim after the generated policy. A malicious or compromised profile can inject `(allow file-read* (subpath "/"))` after `(deny default)`, granting unrestricted read access.

**Impact:** A profile with attacker-controlled `raw` SBPL completely overrides the deny-default semantics on macOS.

**Fix:** Either remove the `raw` field entirely, or restrict it to a hardcoded allowlist of safe SBPL fragments (e.g., specific capability allows). If `raw` must be user-extensible, require explicit opt-in via a config flag and document the security implications.

---

#### H2 — Windows Hook DLL Has No Code Signing Verification
**File:** `src/backends/windows_hook.rs:86–164`, `crates/isol8-winhook/src/lib.rs`  
**CWE:** CWE-347 (Improper Verification of Cryptographic Signature)

Even when the DLL is found at an absolute path beside the binary, no Authenticode signature check is performed before injection via `CreateRemoteThread`. A supply-chain or on-disk replacement of the DLL loads silently.

**Impact:** Any actor who can write to the binary directory compromises all sandboxed sessions.

**Fix:** Verify the DLL's Authenticode signature before calling `LoadLibraryW` (use `WinVerifyTrust`). The preferred long-term fix is to embed the DLL as a PE resource inside the isol8 binary, eliminating the external file entirely.

---

#### H3 — `NtCreateUserProcess` Grandchild Escape on Windows
**File:** `crates/isol8-winhook/src/lib.rs:102–103, 374–431`  
**CWE:** CWE-693 (Protection Mechanism Failure)

The hook intercepts `CreateProcessInternalW` and `CreateProcessA` for grandchild injection but does not hook `NtCreateUserProcess`. A tool that bypasses the Win32 layer (via direct NT syscalls, certain statically linked runtimes, or hand-crafted shellcode) can spawn an unconfined child.

**Impact:** An AI agent compiled against a runtime that issues NT syscalls directly can escape the hook policy.

**Fix:** Hook `NtCreateUserProcess` via MinHook (ntdll export) in addition to the existing `CreateProcess*` hooks. Document this as a hard prerequisite for Tier 1b enforcement. Until fixed, document that hook mode is bypassable by native-NT code.

---

#### H4 — RO Grants Include Execute Rights on Linux
**File:** `src/backends/linux.rs:132, 137`  
**CWE:** CWE-732 (Incorrect Permission Assignment)

`Access::Ro` and `Access::Metadata` both include `Execute` in their Landlock access flags alongside `ReadFile | ReadDir`. Granting `/usr` as `ro` (standard in built-in profiles) automatically allows executing all binaries under `/usr/bin`.

**Impact:** A confined process with RO access to a directory containing privileged utilities can exec them, which may exceed the intended policy.

**Fix:** Document that RO implies Execute on Linux. Consider adding an `ro_noexec` access mode that omits `Execute` for paths that should be readable but not executable.

---

#### H5 — Symlink in Landlock Rule Construction Can Over-Grant
**File:** `src/backends/linux.rs:189–196`  
**CWE:** CWE-59 (Improper Link Resolution)

The `is_dir()` call at rule-build time follows symlinks. If a granted path is a symlink to a directory, the Landlock `O_PATH` file descriptor targets the symlink's destination inode, not the symlink itself. A malicious symlink placed inside a granted directory pointing to a sensitive target outside the grant could broaden the effective policy.

**Impact:** Attacker-controlled symlinks within a granted directory tree can redirect Landlock grants to unintended targets.

**Fix:** Use `std::fs::symlink_metadata()` to detect symlinks before opening a path for Landlock. Either reject symlinks or resolve them to canonical form using `std::fs::canonicalize()` and validate the canonical path is within the intended grant scope.

---

#### H6 — `PR_SET_NO_NEW_PRIVS` Applied Before Landlock Enforcement
**File:** `src/backends/linux.rs:307–311`  
**CWE:** CWE-696 (Incorrect Behavior Order)

In `child_setup_and_exec()`, `set_no_new_privs()` is called first, then `apply_landlock()`. If `apply_landlock()` fails after `no_new_privs` has been set, the process exits with code 127 — but there is a conceptual ordering issue: Landlock should be applied and verified before `no_new_privs` is set, so failure modes are consistent.

**Impact:** Low in practice (the child exits on error), but the ordering violates defense-in-depth. A future refactor that adds a recovery path after the Landlock error could inadvertently leave `no_new_privs` set without Landlock.

**Fix:** Apply and verify Landlock first. Only call `set_no_new_privs()` after `apply_landlock()` returns `Ok(())`.

---

#### H7 — Landlock `BestEffort` Silently Drops Rights on Older Kernels
**File:** `src/backends/linux.rs:250–255`  
**CWE:** CWE-391 (Unchecked Error Condition)

`CompatLevel::BestEffort` is used when building the Landlock ruleset. On kernels that do not support a particular access right, the right is silently dropped without warning. A policy enforced on a new kernel may be substantially weaker on a kernel that lacks certain Landlock ABI features.

**Impact:** Cross-kernel portability silently weakens enforcement. Users assume policy is fully applied; it may not be.

**Fix:** After calling `restrict_self()`, check whether the resulting enforcement level is `FullyEnforced` or `PartiallyEnforced` (the `landlock` crate provides this). Warn the user (or fail) if enforcement is partial, and report which rights were dropped.

---

#### H8 — `/proc` Read Grant Exposes `/proc/self/environ` and Memory Maps
**File:** Built-in Linux profiles (e.g., `linux/system-runtime`)  
**CWE:** CWE-200 (Information Disclosure)

The built-in `linux/system-runtime` profile grants `/proc` as read-only. Landlock's `PathBeneath` grants the entire subtree, so the confined process can read `/proc/self/environ` (parent environment, potentially containing API keys), `/proc/self/maps` (ASLR defeat), and `/proc/[pid]/` for any process visible to the user, leaking secrets across processes.

**Impact:** Information disclosure of secrets from sibling processes and the parent environment. ASLR bypass.

**Fix:** Replace the blanket `/proc` grant with a specific allowlist: `/proc/cpuinfo`, `/proc/stat`, `/proc/meminfo`, `/proc/self/exe`, `/proc/self/fd` (if needed). Explicitly deny `/proc/self/environ`, `/proc/self/maps`, and `/proc/[0-9]*`.

---

#### H9 — Recursive Seed Copy Follows Symlinks at Any Depth
**File:** `src/home.rs:210–230`  
**CWE:** CWE-59 (Improper Link Resolution)

`copy_readonly()` checks for symlinks only on the top-level seed entry. During recursive directory traversal via `read_dir()`, nested symlinks are followed by `std::fs::copy()`, allowing traversal to files outside `$HOME` (e.g., `~/.gitconfig -> /etc/shadow` would copy `/etc/shadow` into the scratch home).

**Impact:** Arbitrary read of files outside the home directory via symlink indirection during seeding.

**Fix:** At every level of recursion, check `symlink_metadata().file_type().is_symlink()` and skip (or error on) symlinks. Never follow symlinks during seed copy.

---

#### H10 — Seed Path TOCTOU Race Condition
**File:** `src/home.rs:191–207`  
**CWE:** CWE-367 (TOCTOU Race Condition)

Between the `src.exists()` check and the `std::fs::copy()` call, an attacker can replace a regular file with a symlink pointing to a sensitive path, causing the copy to follow the new symlink.

**Impact:** Arbitrary file read combined with symlink replacement at the right moment.

**Fix:** Combine with H9's fix — use `symlink_metadata()` to detect and reject symlinks unconditionally, making the TOCTOU irrelevant.

---

#### H11 — `--profile-path` Accepts Untrusted Profiles Without Integrity Check
**File:** `src/profile.rs:344–361`  
**CWE:** CWE-829 (Inclusion of Functionality from Untrusted Control Sphere)

`--profile-path` loads any `.toml` file from a user-supplied directory without signature, hash, or tamper verification. A malicious project directory containing `base.toml` with `access = "rw"` on `/` silently overrides the built-in deny-default profile when the user runs isol8 from that directory.

**Impact:** A cloned repository with attacker-controlled profiles achieves a complete sandbox escape.

**Fix:** Warn prominently when `--profile-path` overrides a built-in layer by the same name. Consider requiring `--allow-custom-profiles` to override built-ins. Document that `--profile-path` is a trusted-admin option, not for untrusted input.

---

#### H12 — Auto-Profile Executable Basename Exploitable by Confined Agent
**File:** `src/filter.rs:39–44`, `src/profile.rs:427–448`  
**CWE:** CWE-610 (Externally Controlled Reference to a Resource)

Auto-profile selection matches the command's `file_stem()` against `filter.executables` in embedded layers. A confined agent that can create a symlink or rename a binary to `claude` (or another agent name) and execute it will trigger selection of the `agents/claude-code` layer, which grants additional paths (`~/.cache/claude`, `~/.config/claude`, etc.) not in the original profile.

**Impact:** A confined agent can elevate its filesystem grants by crafting an executable with a privileged name.

**Fix:** Disable auto-profile selection when `ISOL8_SANDBOXED` is set. The initial launch resolves auto-profiles; the nested invocation should not re-evaluate them.

---

#### H13 — Project `isol8.toml` in CWD Silently Overrides Security Policy
**File:** `src/cli/config.rs:44–89`  
**CWE:** CWE-427 (Uncontrolled Search Path)

Config discovery searches `./isol8.toml` before user or system paths. Running isol8 from an attacker-controlled directory (a cloned repo, a downloaded archive) causes that directory's config to silently set `add_dirs_rw`, `profile_paths`, and other security-relevant fields.

**Impact:** Sandbox escape by placing a permissive `isol8.toml` in any directory the user might invoke isol8 from.

**Fix:** Warn when config is loaded from the CWD. Consider a `--no-cwd-config` flag (or making it opt-in via a `ISOL8_ALLOW_CWD_CONFIG=1` env var). Document that CWD config is treated as fully trusted.

---

### Medium

---

#### M1 — Windows `%VAR%` Expansion Enables Indirect Path Traversal
**File:** `src/home.rs:162–185`  
**CWE:** CWE-426 (Untrusted Search Path)

`expand_windows_vars()` expands `%TEMP%`, `%APPDATA%`, and similar variables from the host environment. An attacker who controls the host environment (compromised shell, malicious batch file, container with custom env) can set `%TEMP%=C:\Windows\System32` and redirect a grant on `%TEMP%\work` to the system directory.

**Fix:** Only expand OS-determined variables (`%SYSTEMROOT%`, `%SYSTEMDRIVE%`) via the Windows API (`SHGetKnownFolderPath`). For user directories, validate the expanded result starts with an expected prefix before applying the grant.

---

#### M2 — Policy Block Union Semantics May Undermine Deny-First Merge
**File:** `src/filter.rs:79–101`, `src/profile.rs:536–656`  
**CWE:** CWE-269 (Improper Privilege Management)

Within `apply_policies()`, paths from matching policy blocks are `extend()`-ed onto the layer before the main deny-first merge runs. If a lower-priority layer's policy block adds a path that a higher-priority layer denies with `Access::None`, the union step may re-introduce the grant before the merge resolves it — depending on key ordering.

**Fix:** Apply the same `(path, match_kind)` key-based merge logic to policy block paths as the main merge uses, ensuring deny-first semantics hold at every stage.

---

#### M3 — `MatchKind::Literal/Prefix/Regex` Silently Skipped on Linux
**File:** `src/backends/linux.rs:182–186`  
**CWE:** CWE-636 (Not Failing Securely)

Linux Landlock only supports path-beneath subtree rules. Grant entries using `Literal`, `Prefix`, or `Regex` match kinds (valid for macOS Seatbelt) are silently skipped on Linux, becoming no-ops. A profile author who tests on macOS may believe a literal grant is enforced on Linux when it is not.

**Fix:** Log a warning (or emit a `--show-policies` annotation) when a non-`Subpath` match kind is encountered on Linux. In a strict mode, treat this as an error.

---

#### M4 — HOME Namespace Helpers Disabled; `getpwuid()` Returns Real Home
**File:** `src/backends/linux.rs:354, 364, 386` (dead code)  
**CWE:** CWE-200 (Information Disclosure)

The namespace functions `unshare_user_and_mount_ns()`, `write_uid_gid_mappings()`, and `bind_mount_home()` exist but are never called. Without a mount namespace, a tool that calls `getpwuid(getuid())` or reads `/etc/passwd` learns the real home directory path, partially defeating `--home` replacement.

**Fix:** Re-enable namespace helpers with a runtime check for `/proc/self/uid_map` writability. Cache the result at startup. If namespaces are unavailable, document the limitation in `--dry-run` output.

---

#### M5 — `@diag` Exposes Policy Details to a Confined Agent
**File:** `src/diag.rs:48–102`  
**CWE:** CWE-200 (Information Disclosure)

The `@diag` meta-command uses delta-debugging to reveal exactly which paths are required for a command to launch. A sandboxed agent that can exec `isol8 @diag <command>` learns the minimum required grants and can use this information to refine an escape strategy.

**Fix:** Check `ISOL8_SANDBOXED` at the start of `diag::run()` and bail with a clear error. `@diag` is a developer tool and should not be available inside the sandbox.

---

#### M6 — `ISOL8_SANDBOXED` Nesting Marker Evadable on Linux
**File:** `src/sandbox.rs:305–310`, `src/env.rs:82–83`  
**CWE:** CWE-693 (Protection Mechanism Failure)

The nesting check relies solely on the `ISOL8_SANDBOXED` environment variable. On Linux, a confined process can call `unsetenv("ISOL8_SANDBOXED")` before exec'ing a new `isol8` invocation, bypassing the check. macOS Seatbelt can deny `process-write-environment`, but Landlock has no equivalent.

**Fix:** Supplement the env var with a platform-specific marker: on Linux, write a sentinel file to the scratch home (`$HOME/.isol8_sandbox`) that the Landlock policy makes read-only, and check for it in addition to the env var.

---

#### M7 — Unrestricted macOS Capabilities Grant Broad Mach Access
**File:** `src/backends/macos.rs:202–277`  
**CWE:** CWE-269 (Improper Privilege Management)

The `capabilities` field allows profiles to grant Seatbelt capability names (e.g., `mach-lookup`) without any per-service or per-path restriction. A profile that grants `mach-lookup` gives the confined process access to any Mach service registered in the bootstrap namespace, which can include privileged system daemons.

**Fix:** Where possible, restrict capability grants to specific service names rather than the capability class. Document that `mach-lookup` is a broad grant equivalent to `allow network-outbound` on macOS.

---

#### M8 — Incomplete macOS Symlink Pairs for `/tmp` and `/var`
**File:** `src/backends/macos.rs:284–304`  
**CWE:** CWE-59 (Improper Link Resolution)

The macOS backend emits duplicate SBPL rules for known symlink pairs (`/tmp` → `/private/tmp`, `/var` → `/private/var`). However, if additional system symlinks exist (e.g., `/home` → `/System/Volumes/Data/home` on some configurations), they are not covered, and access may be denied or unintentionally granted depending on which path Seatbelt resolves.

**Fix:** Enumerate the full set of macOS canonical symlinks or use `realpath()` to canonicalize grant paths and emit both the original and canonical forms unconditionally.

---

#### M9 — Windows 8.3 Short Names and NTFS Junctions Bypass Path Policy
**File:** `crates/isol8-path-policy/src/lib.rs:94–105`  
**CWE:** CWE-706 (Use of Incorrectly-Resolved Name or Reference)

The path-matching logic does not canonicalize 8.3 short names (e.g., `C:\PROGRA~1` for `C:\Program Files`) or NTFS junctions/symlinks. A grant on `C:\Program Files\app` does not block access via `C:\PROGRA~1\app`. This is documented but not enforced.

**Fix:** In the hook's path normalization, call `GetLongPathNameW` to expand 8.3 names and `GetFinalPathNameByHandleW` to resolve junctions. Apply these before policy matching.

---

#### M10 — Handle Inheritance Bypass in Windows Hook
**File:** `src/backends/windows.rs:140–158`  
**CWE:** CWE-403 (Exposure of Sensitive Information Through Use of Inherited Handle)

The hook's `confine_created_process` does not force `bInheritHandles = FALSE` for grandchildren. A confined process that spawns a grandchild with `bInheritHandles = TRUE` can pass open file handles (to files outside its grants, or network sockets) to the grandchild, bypassing hook enforcement.

**Fix:** In the hook's `CreateProcessInternalW` detour, unconditionally set `bInheritHandles = FALSE` before calling the original function.

---

#### M11 — Long Filename Buffer Scan Out-of-Bounds Risk
**File:** `crates/isol8-winhook/src/lib.rs:715–739`  
**CWE:** CWE-125 (Out-of-Bounds Read)

`wide_len()` and `narrow_len()` scan for null terminators in raw pointer buffers up to 32,768 characters without receiving a known buffer length from the caller. If the Windows API passes a non-null-terminated buffer (a documented edge case in some NT internal APIs), the scan reads past the allocation.

**Fix:** Accept a `maxlen` parameter from the caller (derived from the Win32 API's documented buffer constraints) rather than scanning open-endedly.

---

#### M12 — Memory-Mapped I/O Not Hooked on Windows
**File:** `crates/isol8-winhook/src/lib.rs` (documented gap)  
**CWE:** CWE-284 (Improper Access Control)

`CreateFileMappingW` and `MapViewOfFile` are not hooked. A process that opens a file (which is checked), then maps it with write access, can modify it regardless of the `ro` grant enforced at the `CreateFile` level.

**Fix:** Hook `CreateFileMappingW` and deny `PAGE_READWRITE`/`PAGE_WRITECOPY` mappings on files opened under a read-only grant.

---

#### M13 — `ISOL8_*` Env Vars Silently Override Config
**File:** `src/cli/config.rs:143–185`  
**CWE:** CWE-15 (External Control of System or Configuration Setting)

Variables like `ISOL8_ADD_DIRS_RW=/` override config file settings silently, with no warning. A compromised parent process or injected `.bashrc` can widen sandbox grants without any indication to the user.

**Fix:** At startup, log which `ISOL8_*` vars are active (at least in verbose mode). Consider a `--strict-config` flag that disallows env overrides entirely.

---

#### M14 — `--env-pass` Can Expose Host Secrets to Confined Agent
**File:** `src/env.rs:69–79`  
**CWE:** CWE-200 (Information Disclosure)

`--env-pass` forwards named host environment variables into the sandbox without any warning. A user who passes `--env-pass GITHUB_TOKEN` may not realize the sandboxed agent can read and exfiltrate that token.

**Fix:** Log passed variables at startup. Optionally warn when a variable name matches a secret heuristic (`*TOKEN`, `*SECRET`, `*KEY`, `*PASSWORD`).

---

### Low

---

#### L1 — WSL2 9P Filesystem Enforcement Uncertain
**File:** `_docs/linux-support.md:179–184`  
There is no runtime detection of 9P/drvfs filesystems mounted at `/mnt/c`. Users may unknowingly rely on Landlock to protect Windows-side files, but 9P filesystems may not honor Landlock.

**Fix:** Detect and warn when a granted path resides on a 9P mount. Explicitly document the gap.

---

#### L2 — Registry Access Unhooked on Windows
**File:** `crates/isol8-winhook/src/lib.rs` (documented gap)  
Registry APIs are not intercepted. A confined process can read/write HKCU and (if privileged) HKLM, including credential stores and startup keys.

**Fix:** Hook `RegOpenKeyExW` and `RegCreateKeyExW` with a deny-by-default policy, or recommend AppContainer mode for deployments where registry isolation is required.

---

#### L3 — Error Messages Leak Full Filesystem Paths
**File:** `src/cli/config.rs:100, 531–534`, `src/profile.rs:390–391`  
Error messages include full absolute paths (home dir, config paths, profile paths), which can leak filesystem layout to a sandboxed agent that triggers an error.

**Fix:** Use relative paths or just filenames in user-facing error messages. Reserve full paths for debug/verbose output.

---

#### L4 — Landlock ABI Probe Uses Hardcoded Syscall Number
**File:** `src/backends/linux.rs:216–223`  
`probe_landlock_abi()` issues a raw `syscall(444, ...)` instead of using the `landlock` crate's API. Syscall number 444 is correct for x86_64/arm64/riscv64 but is hardcoded.

**Fix:** Prefer the `landlock` crate's probe if available. If not, document the assumption explicitly.

---

#### L5 — Windows Large-Policy File Fallback Never Wired
**File:** `src/backends/windows.rs:114–121`, `crates/isol8-winhook/src/lib.rs:195–205`  
The DLL supports reading the policy from `ISOL8_PATH_POLICY_FILE` when the inline env var is too large, but the launcher never sets this variable. Very large policies would silently fail to load (fail-closed, but confusing).

**Fix:** In `launch_with_hook`, if the serialized policy exceeds a threshold (e.g., 16 KB), write it to a temp file and set `ISOL8_PATH_POLICY_FILE` instead.

---

#### L6 — Windows Policy Load Failure Produces No Diagnostic
**File:** `crates/isol8-winhook/src/lib.rs:203–205`  
JSON parse failures in `DllMain` return `FALSE` silently, causing `LoadLibraryW` to fail with a generic error. There is no log file, event log entry, or stdout message indicating why.

**Fix:** Write a brief error to `%TEMP%\isol8-hook-error.log` on initialization failure to aid debugging.

---

#### L7 — macOS Policy Rejection Leaks Policy Text
**File:** `src/backends/macos.rs:85–95`  
When `sandbox-exec` exits with code 65 (policy compilation error), the full generated SBPL is echoed in the error message. This includes home paths, temp paths, and all granted paths.

**Fix:** Gate the policy-text display on `--verbose` or `--dry-run`. In non-verbose mode, show only the error summary.

---

#### L8 — Profile Layer Name Shadowing of Builtins
**File:** `src/profile.rs:352–356`  
A `--profile-path` TOML file named `base.toml` silently replaces the built-in `base` layer with no warning.

**Fix:** Warn when a profile-path entry shadows a built-in layer by name.

---

## Positive Findings / Correctly Handled

| Area | Detail |
|------|--------|
| Deny-first merge | `Access::None` at any layer correctly overrides lower layers; the highest-explicit-wins logic is sound |
| TOML safety | All structs use `#[serde(deny_unknown_fields)]`; typos are rejected, not silently ignored |
| SBPL string escaping | Profile values are escaped before SBPL emission; no obvious injection via path strings |
| Environment sanitization | The env allowlist is conservative; `HOME` is applied first and overrides any passthrough |
| Dependency cycle detection | `requires` cycles produce a clean error with the cycle path included |
| Executable resolution | `cmd[0]` is resolved via host `PATH` before spawn; `CommandNotFound` error is clean |
| Windows `..` traversal | Path traversal via `..` in `normalize_path` is explicitly rejected (fixed in current branch) |
| Windows case folding | Full path lowercasing is applied before policy matching |
| Grandchild injection | `CreateProcessInternalW` is hooked to propagate the DLL to grandchildren |
| Fail-closed on Windows | A DLL initialization failure terminates the child rather than proceeding without policy |
| `no_new_privs` | Set in every Linux child regardless of Landlock status |

---

## Risk Summary

| ID | Title | Severity | Platform | Status |
|----|-------|----------|----------|--------|
| C1 | Empty Landlock ruleset bypasses enforcement | **Critical** | Linux | ✅ Fixed |
| C2 | Windows hook DLL hijacking via CWD | **Critical** | Windows | ✅ Fixed |
| C3 | Seed `..` path traversal outside home | **Critical** | All | ✅ Fixed |
| H1 | Raw SBPL passthrough injection | High | macOS | Open |
| H2 | No DLL code signing / integrity check | High | Windows | Open |
| H3 | `NtCreateUserProcess` grandchild escape | High | Windows | Open |
| H4 | RO grants include execute rights | High | Linux | Open |
| H5 | Symlink over-grant in Landlock rules | High | Linux | Open |
| H6 | `no_new_privs` ordered before Landlock | High | Linux | Open |
| H7 | `BestEffort` silently drops access rights | High | Linux | Open |
| H8 | `/proc` grant exposes `/proc/self/environ` | High | Linux | Open |
| H9 | Recursive seed copy follows symlinks | High | All | Open |
| H10 | Seed TOCTOU race | High | All | Open |
| H11 | `--profile-path` accepts untrusted profiles | High | All | Open |
| H12 | Auto-profile basename exploitable | High | All | Open |
| H13 | CWD `isol8.toml` silently overrides policy | High | All | Open |
| M1 | Windows `%VAR%` expansion injection | Medium | Windows | Open |
| M2 | Policy block union undermines deny-first | Medium | All | Open |
| M3 | `Literal/Prefix/Regex` silently no-op on Linux | Medium | Linux | Open |
| M4 | HOME namespace helpers disabled | Medium | Linux | Open |
| M5 | `@diag` leaks policy to confined agent | Medium | macOS | Open |
| M6 | `ISOL8_SANDBOXED` marker evadable | Medium | Linux | Open |
| M7 | Unrestricted macOS capability grants | Medium | macOS | Open |
| M8 | Incomplete macOS symlink pairs | Medium | macOS | Open |
| M9 | Windows 8.3 short names bypass policy | Medium | Windows | Open |
| M10 | Handle inheritance bypass in hook | Medium | Windows | Open |
| M11 | Long filename buffer scan OOB | Medium | Windows | Open |
| M12 | Memory-mapped I/O not hooked | Medium | Windows | Open (documented) |
| M13 | `ISOL8_*` env vars silent override | Medium | All | Open |
| M14 | `--env-pass` exposes host secrets | Medium | All | Open |
| L1 | WSL2 9P filesystem enforcement gap | Low | Linux/WSL2 | Documented |
| L2 | Registry access unhooked | Low–Med | Windows | Documented |
| L3 | Error messages leak paths | Low | All | Open |
| L4 | Landlock ABI probe hardcoded syscall | Low | Linux | Open |
| L5 | Large-policy file fallback not wired | Low | Windows | Open |
| L6 | Windows policy load failure silent | Low | Windows | Open |
| L7 | macOS policy rejection leaks policy text | Low | macOS | Open |
| L8 | Profile layer name shadows builtin | Low | All | Open |

---

## Recommended Priority Order

**Fixed:** C1, C2, C3 (and the symlink component of H9/H10 in `copy_readonly`).

Remaining open items, by priority:

1. **H9/H10** — Seed TOCTOU race (the `..` check is fixed; the race between `exists()` and `copy()` remains — combine with the symlink fix already applied)
2. **H1** — Remove or strictly gate the SBPL `raw` field
3. **H12 + H13** — Disable auto-profile inside sandbox; warn on CWD config load
4. **H11** — Add warning when `--profile-path` shadows a built-in layer
5. **M5/M6** — Block `@diag` inside sandbox; harden nesting marker
6. **H2** — Implement DLL Authenticode verification or resource-embed the DLL
7. **H3** — Hook `NtCreateUserProcess` for complete grandchild coverage

---

## Test Coverage Status

### Fixed findings — tests in place

| Finding | Code fix | Unit/regression test | Field test |
|---------|----------|----------------------|------------|
| **C1** Empty Landlock ruleset bypasses enforcement | `src/backends/linux.rs` — removed early-return before `restrict_self()` | `backends::linux::tests::build_rules_empty_profile` (build side); field scenario 17 (enforcement side) | Scenario **17** `linux-zero-grant-deny-all` (`src/bin/isol8-field-test.rs`) |
| **C2** Windows DLL hijacking via CWD | `src/backends/windows_hook.rs` — removed bare-filename fallback | `backends::windows_hook::tests::hook_dll_search_paths_are_all_absolute` | — (no meaningful field test possible without a real malicious DLL) |
| **C3a** Seed `..` traversal | `src/home.rs` — rejects `..`/absolute in `seed()` | `home::tests::seed_rejects_dotdot_traversal` | — |
| **C3b** Seed follows symlinks | `src/home.rs` — `copy_readonly()` skips symlinks at all depths | `home::tests::seed_skips_symlinks` | — |

### Open findings — tests needed when fixed

When each of the following findings is addressed, a test of the indicated kind should ship alongside the fix:

| Finding | Suggested test kind | What to verify |
|---------|---------------------|----------------|
| **H1** Raw SBPL injection | Unit (macOS backend) | A profile with `raw = "(allow file-read* (subpath /))"` must not grant unrestricted read; the field should be validated or sanitized |
| **H2** No DLL code signing | Unit (Windows) | `inject_dll_and_resume` must fail or warn if the DLL is not Authenticode-signed |
| **H3** `NtCreateUserProcess` bypass | Field (Windows, scenario 18) | A confined process that calls `NtCreateUserProcess` directly must not spawn an unhooked grandchild |
| **H4** RO grants include Execute | Unit (Linux backend) | `Access::Ro` flags for a non-executable path should not include `Execute` (or document the deliberate choice) |
| **H5** Symlink over-grant in Landlock | Unit (Linux backend) | A symlink to outside the intended grant tree should not receive a Landlock rule for its target |
| **H6** `no_new_privs` before Landlock | Unit (Linux backend) | Verify call order: `apply_landlock` must succeed before `set_no_new_privs` is called |
| **H7** `BestEffort` silently drops rights | Unit (Linux backend) | After `restrict_self()`, warn or fail when `RulesetStatus::PartiallyEnforced` — add a test that triggers partial enforcement on a mock ABI version |
| **H8** `/proc` exposes `environ`/maps | Integration (profile) | `linux/system-runtime` profile should not grant the full `/proc` subtree; test that `/proc/self/environ` is inaccessible in a confined process |
| **H9/H10** Seed TOCTOU race | Unit (home.rs) | Use `O_NOFOLLOW`-style atomic checks; verify the race window is closed |
| **H11** `--profile-path` shadows builtin | Unit (cli) | Loading a profile-path TOML named `base.toml` should emit a warning |
| **H12** Auto-profile basename exploit | Unit (filter + sandbox) | When `ISOL8_SANDBOXED` is set, `resolve_auto_profiles` must return no additional layers |
| **H13** CWD `isol8.toml` overrides policy | Integration | Loading from CWD should log a warning; `--no-cwd-config` should skip it entirely |
| **M2** Policy block union over-grants | Unit (filter) | A `[[policies]]` block on a lower layer cannot widen an `Access::None` set by a higher layer |
| **M3** Non-Subpath grants silently no-op on Linux | Unit (Linux backend) | A Literal-match grant on Linux must produce a warning or error, not silently vanish |
| **M5** `@diag` leaks policy to agent | Unit (cli/diag) | Running `@diag` with `ISOL8_SANDBOXED` set must return an error |
| **M6** `ISOL8_SANDBOXED` marker evadable | Integration (Linux) | A field scenario that unsets `ISOL8_SANDBOXED` and re-invokes isol8 must be blocked by the platform-level marker (once implemented) |
| **M9** Windows 8.3 short names bypass | Unit (isol8-path-policy) | `C:\PROGRA~1\app\secret.txt` must be denied when only `C:\Program Files\app\safe.txt` is granted |
| **M10** Handle inheritance bypass | Unit (Windows hook) | `confine_created_process` must set `bInheritHandles = FALSE` regardless of the child's request |
| **M12** Memory-mapped I/O not hooked | Unit (isol8-winhook) | `CreateFileMappingW` with write access on an ro-granted file must be denied |
