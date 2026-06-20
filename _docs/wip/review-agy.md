# isol8 Code Audit & Security Review

**Date:** June 20, 2026  
**Auditor:** Antigravity (Gemini 3.5 Flash)  
**Scope:** Core implementation audit of [isol8](file:///Users/mdn/works/projects/agent-manager/workspace/isol8) (Phase 1, macOS MVP working, Linux Landlock partial)  
**Target File:** `_docs/wip/review-agy.md`

---

## Executive Summary

An audit of the [isol8](file:///Users/mdn/works/projects/agent-manager/workspace/isol8) codebase was conducted to evaluate its safety, soundness, and correctness. 

The core architecture is elegant and robust: the profile-driven, deny-first inheritance model is sound, and the macOS Seatbelt backend is of high quality. However, several critical correctness bugs and substantial security gaps exist in both the HOME replacement pipeline and the Linux Landlock backend.

The most severe findings include:
1. **Linux Confinement Deficiencies**: Mount namespaces and user namespaces are not activated, rendering the `$HOME` replacement trivial to bypass.
2. **Ancestor Read Leakage on Linux**: Parent directory traversal grants full read access to user homes.
3. **Correctness Bugs**: Tilde expansion is missing for CLI-specified homes, and the YAML config generator outputs TOML.
4. **macOS Profile Syntax Errors**: The built-in `process-control` profile contains invalid SBPL syntax that causes sandbox initialization failure.

---

## Severity Assessment Table

| ID | Severity | Component | Summary | Status / Action |
|:---|:---|:---|:---|:---|
| **[SEC-01](#sec-01-critical--incomplete-home-replacement-on-linux-namespaces-disabled)** | **CRITICAL** | `backends::linux` | Incomplete HOME replacement (namespaces dead code) | Must fix |
| **[SEC-02](#sec-02-critical--ancestor-read-leakage-on-linux)** | **CRITICAL** | `backends::linux` | Ancestor traversal rules leak read access to user home | Must fix |
| **[BUG-01](#bug-01-high--tilde-expansion-missing-on---home)** | **HIGH** | `home` | `--home ~/path` fails to expand tilde | Must fix |
| **[BUG-02](#bug-02-high--invalid-sbpl-syntax-in-process-controltoml)** | **HIGH** | `profiles` | Invalid SBPL syntax in `process-control.toml` crashes macOS | Must fix |
| **[SEC-03](#sec-03-high--scratch-home-path-is-predictable-and-hijackable)** | **HIGH** | `home` | Scratch HOME temp path is predictable and hijackable | Must fix |
| **[BUG-03](#bug-03-medium--homereplaceenabled-flag-ignored)** | **MEDIUM** | `home` | `HomeReplace.enabled` is ignored during resolution | Should fix |
| **[BUG-04](#bug-04-medium--init_template-yaml-generator-outputs-toml)** | **MEDIUM** | `config` | YAML initialization template outputs TOML format | Should fix |
| **[SEC-04](#sec-04-medium--silently-ignoring-matchkind-constraints-on-linux)** | **MEDIUM** | `backends::linux` | Silently ignores `literal`, `prefix`, `regex` grants on Linux | Should fix |
| **[SEC-05](#sec-05-medium--unsafe-multithreaded-fork-in-linux-backend)** | **MEDIUM** | `backends::linux` | Manual `fork` is unsafe in multithreaded library contexts | Should fix |
| **[SEC-06](#sec-06-medium--no-network-confinement-by-default)** | **MEDIUM** | `profiles` | Outbound network is completely unconfined by default | Should fix |
| **[BUG-05](#bug-05-low--non-deterministic-auto-select-ordering)** | **LOW** | `profile` | Layer selection order is non-deterministic (HashMap) | Hardening |
| **[BUG-06](#bug-06-low--config-apply_to_run-cannot-disable-auto_profiles)** | **LOW** | `config` | Configuration OR logic prevents disabling `auto_profiles` | Hardening |

---

## Detailed Findings

### SEC-01 [CRITICAL] — Incomplete HOME Replacement on Linux (Namespaces Disabled)
- **File:** [linux.rs:275-301](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L275-L301)
- **Description:** The Linux child process setup in `child_setup_and_exec` takes `_effective_home` but never invokes the namespace helper functions. The helpers [unshare_user_and_mount_ns](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L329-L334), [write_uid_gid_mappings](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L339-L356), and [bind_mount_home](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L361-L379) are marked `#[allow(dead_code)]`.
- **Impact:** HOME replacement (R4) is only enforced via the `$HOME` environment variable. Any sandboxed process that bypasses the env var (e.g., using `getpwuid` or hardcoded paths like `/home/user`) can access and modify the real user's home directory.
- **Recommendation:** Wire up user namespace, mount namespace, and bind-mounting. If `uid_map` write fails (e.g. inside restrictive VMs), log a warning and fall back to Landlock-only mode gracefully.

### SEC-02 [CRITICAL] — Ancestor Read Leakage on Linux
- **File:** [linux.rs:180-210](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L180-L210)
- **Description:** To allow path resolution, the Linux backend walks the parent path hierarchy of each grant and appends ancestor rules. However, it grants them `AccessFs::{ReadFile | ReadDir}`:
  ```rust
  rules.push(LandlockRule {
      path: anc,
      access: make_bitflags!(AccessFs::{ReadFile | ReadDir}),
  });
  ```
- **Impact:** If a user grants read-write access to a workspace directory (e.g. `/home/user/projects/workspace`), the sandbox is granted full read-only access to all parent directories: `/home/user/projects`, `/home/user`, and `/home`. The process can read sensitive configuration files, SSH keys, and personal documents.
- **Root Cause & Correction:** Landlock does **not** require explicit rules on parent directories for path traversal to a permitted subdirectory. Traversal is allowed implicitly. Ancestor rules are only needed if `getcwd()` or parent path traversal must resolve directory names. If parent rules are added, they should never grant content access (`ReadFile | ReadDir`).

### BUG-01 [HIGH] — Tilde Expansion Missing on `--home`
- **File:** [home.rs:43-55](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/home.rs#L43-L55)
- **Description:** When the user supplies a replacement home via CLI `--home ~/scratch` or `ISOL8_HOME=~/scratch`, the path is converted directly to a `PathBuf` without tilde expansion.
- **Impact:** The effective home becomes the literal directory `~/scratch` relative to the current working directory, which breaks subsequent tilde expansions (generating paths like `~/scratch/.cargo`).
- **Fix:** Apply [home::expand_tilde](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/home.rs#L70-L78) to the CLI `--home` argument.
  ```diff
  -    let path = if let Some(home) = run.home() {
  -        PathBuf::from(home)
  +    let path = if let Some(home) = run.home() {
  +        PathBuf::from(home::expand_tilde(home, &real_home()))
  ```

### BUG-02 [HIGH] — Invalid SBPL Syntax in `process-control.toml`
- **File:** [process-control.toml:13](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/profiles/integrations/process-control.toml#L13)
- **Description:** The raw SBPL block contains the line:
  ```text
  (kill, pkill, kill -0)
  ```
- **Impact:** This is invalid SBPL syntax. When a user runs a command on macOS that includes this profile, `sandbox-exec` fails to compile the policy and exits with code 65, crashing the wrapper process.
- **Fix:** Remove the invalid line or turn it into a Lisp-style comment prefixing with semicolons (`;; (kill, pkill, kill -0)`).

### SEC-03 [HIGH] — Scratch HOME Path is Predictable and Hijackable
- **File:** [home.rs:48](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/home.rs#L48)
- **Description:** Scratch homes are generated in the shared `/tmp` directory using the wrapper's process ID:
  ```rust
  let dir = std::env::temp_dir().join(format!("isol8-{}-home", std::process::id()));
  ```
- **Impact:** On shared multi-user systems, an attacker can predict the next process ID and pre-create the target directory or symlink, allowing them to intercept or hijack the sandboxed process's configuration/seeding directories.
- **Fix:** Introduce a cryptographically secure random sequence (e.g., via the `tempfile` crate or a random `u64` generator) and use `std::fs::create_dir` (which fails if the directory already exists).

### BUG-03 [MEDIUM] — `HomeReplace.enabled` Flag Ignored
- **File:** [home.rs:26-41](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/home.rs#L26-L41)
- **Description:** The `HomeReplace` schema contains an `enabled` boolean, but [home::resolve](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/home.rs#L26) only inspects `path` and `auto_scratch` when resolving layers.
- **Impact:** Setting `enabled = false` in a profile has no effect; if `auto_scratch = true` is set, a replacement home will still be created.
- **Fix:** Verify the `enabled` field in the layer loop:
  ```rust
  if let Some(hr) = &layer.home_replace {
      if hr.enabled {
          hr_path = hr.path.clone();
          auto_scratch = hr.auto_scratch;
          // ...
      }
  }
  ```

### BUG-04 [MEDIUM] — `init_template` YAML Generator Outputs TOML
- **File:** [config.rs:171-199](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/config.rs#L171-L199)
- **Description:** The YAML generator branch in [init_template](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/config.rs#L171) returns identical TOML content format as the TOML branch.
- **Impact:** Running `isol8 @init --format yaml` creates a file that fails to parse as YAML.
- **Fix:** Update the YAML string representation to use standard YAML formatting:
  ```yaml
  # isol8 configuration
  default_profiles:
    - base
    - macos/system-runtime
  auto_profiles: true
  profile_paths: []
  add_dirs_rw: []
  add_dirs_ro: []
  ```

### SEC-04 [MEDIUM] — Silently Ignoring MatchKind Constraints on Linux
- **File:** [linux.rs:157-178](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L157-L178)
- **Description:** The Landlock builder silently ignores `literal`, `prefix`, and `regex` match kind path grants, claiming Landlock only supports subtree (`subpath`) grants.
- **Impact:** Security rules configured to grant narrow access to specific files (e.g. read-only on `/etc/hosts` literal) are silently discarded, resulting in unexpected permissions errors for the sandboxed process.
- **Fix & Support:** 
  - `MatchKind::Literal` **can** be supported by Landlock: opening a file FD (rather than its parent directory) and registering it as a `PathBeneath` rule restricts the rule to that file only.
  - For unsupported matchers (`regex`, `prefix`), the backend should log a warning or bail, rather than silently ignoring the rule.

### SEC-05 [MEDIUM] — Unsafe Multithreaded `fork` in Linux Backend
- **File:** [linux.rs:50-70](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L50-L70)
- **Description:** The Linux backend invokes `nix::unistd::fork()` manually and executes memory allocations (like `Command::new()`) in the child.
- **Impact:** Forking a multithreaded process without calling `exec` immediately is unsafe, as lock states from other threads are copied in their locked state, leading to deadlocks. If `isol8` is ever integrated as a library, this manual fork will cause intermittent deadlocks.
- **Fix:** Leverage the standard library's `std::process::Command` and execute Landlock/namespace setup inside the `CommandExt::pre_exec` hook, which runs safely in the child after fork but before exec.

### SEC-06 [MEDIUM] — No Network Confinement by Default
- **Files:** [network.toml](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/profiles/network.toml), [macos.rs](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/macos.rs)
- **Description:** The default sandbox policies on macOS leave outbound network fully open. There is no implicit network deny.
- **Impact:** A sandboxed process can connect to any host on the internet and exfiltrate data, making process isolation less effective.
- **Fix:** State in the documentation that Phase 1 does not restrict networks. In Phase 3, default to `(deny network-outbound)` and require explicit opt-in network layers.

### BUG-05 [LOW] — Non-Deterministic Auto-Select Ordering
- **File:** [profile.rs:323-334](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/profile.rs#L323-L334)
- **Description:** Auto-selected profiles are gathered by iterating a `HashMap` registry.
- **Impact:** HashMap iteration order is randomized in Rust. If multiple auto-selected layers are matched, they can be merged in different orders, leading to non-deterministic behavior.
- **Fix:** Collect names into a sorted vector before resolving requires.

---

## Technical Debt & Fragility

### Landlock Path Existence Fragility
- **File:** [linux.rs:232-238](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L232-L238)
- **Issue:** `apply_landlock` attempts to open a `PathFd` on every path in the policy. If *any* path does not exist (e.g. optional caching paths or systems directories), `PathFd::new` fails with `ENOENT`, crashing the CLI wrapper.
- **Fix:** If a path does not exist, check if it's a read grant and silently skip it. Landlock default-deny will block it if created later anyway.

---

## Recommendations & Next Steps

> [!IMPORTANT]
> The immediate development focus should be on resolving critical bugs affecting correctness and resolving the security gaps in the Linux backend.

### Action Plan
1. **Fix CLI Home Tilde Expansion**: Fix `BUG-01` to ensure `--home` paths resolve correctly.
2. **Implement User & Mount Namespaces on Linux**: Re-enable and test the namespace code in `src/backends/linux.rs` using `CommandExt::pre_exec`.
3. **Correct Ancestor Logic on Linux**: Strip parent traversal directory rules of `ReadFile`/`ReadDir` permissions.
4. **Fix config template format**: Ensure `yaml` output uses valid YAML formatting syntax.
5. **Clean up `process-control.toml`**: Comment out or remove the shell string to prevent macOS sandbox compilation failure.
