# isol8 Code Audit & Security Review

**Date:** June 20, 2026  
**Auditor:** Antigravity (Gemini 3.5 Flash)  
**Scope:** Core implementation audit of [isol8](file:///Users/mdn/works/projects/agent-manager/workspace/isol8) (Phase 1, macOS MVP working, Linux Landlock/namespaces deferred/ignored)  
**Target File:** `_docs/wip/review-agy.md`

---

## Executive Summary

An audit of the [isol8](file:///Users/mdn/works/projects/agent-manager/workspace/isol8) codebase was conducted to evaluate its safety, soundness, and correctness. 

Per current project directives, **the Linux backend is ignored for this review cycle** and its findings have been moved to a separate [Deferred Linux Considerations](#deferred-linux-considerations-work-in-progress) section. The primary focus of this review is on the active **macOS Seatbelt backend**, shared CLI argument parsing, configuration loading, tilde expansion, and the embedded profile model.

The core architecture is elegant and robust. However, several critical bugs and security issues in the active macOS backend and the profiles layer must be resolved before a production release:
1. **Critical Profile Breakage (Missing `/` Grant)**: The macOS system-runtime profile is missing the mandatory root directory literal grant, causing *all* commands to crash with a SIGABRT (exit 134) on startup.
2. **Broken Safehouse Macro Support**: Multiple integration profiles use Scheme helpers like `home-literal`, `home-subpath`, and `HOME_DIR`. Since `isol8` does not define these in its Seatbelt header, all of these profiles fail to compile, crashing with exit code 65.
3. **CLI and Path Expansion Bugs**: Tilde expansion is missing for CLI-specified homes, and the YAML config generator incorrectly outputs TOML format.
4. **macOS Profile Syntax Error**: The built-in `process-control` profile contains invalid SBPL syntax that causes sandbox initialization failure.

---

## Active Severity Assessment Table (macOS & CLI Core)

| ID | Severity | Component | Summary | Status / Action |
|:---|:---|:---|:---|:---|
| **[BUG-01](#bug-01-critical--missing-root-directory--grant-in-macos-system-runtime)** | **CRITICAL** | `profiles` | Missing `/` literal grant causes SIGABRT on macOS | Must fix |
| **[BUG-02](#bug-02-critical--missing-sbpl-scheme-macro-helpers-for-safehouse-profiles)** | **CRITICAL** | `backends::macos` | Undefined `home-literal`, `home-subpath`, `HOME_DIR` crash profiles | Must fix |
| **[BUG-03](#bug-03-high--tilde-expansion-missing-on---home)** | **HIGH** | `home` | `--home ~/path` fails to expand tilde | Must fix |
| **[BUG-04](#bug-04-high--invalid-sbpl-syntax-in-process-controltoml)** | **HIGH** | `profiles` | Invalid SBPL syntax in `process-control.toml` crashes macOS | Must fix |
| **[SEC-01](#sec-01-high--scratch-home-path-is-predictable-and-hijackable)** | **HIGH** | `home` | Scratch HOME temp path is predictable and hijackable | Must fix |
| **[BUG-05](#bug-05-medium--homereplaceenabled-flag-ignored)** | **MEDIUM** | `home` | `HomeReplace.enabled` is ignored during resolution | Should fix |
| **[BUG-06](#bug-06-medium--init_template-yaml-generator-outputs-toml)** | **MEDIUM** | `config` | YAML initialization template outputs TOML format | Should fix |
| **[SEC-02](#sec-02-medium--no-network-confinement-by-default)** | **MEDIUM** | `profiles` | Outbound network is completely unconfined by default | Should fix |
| **[BUG-07](#bug-07-low--non-deterministic-auto-select-ordering)** | **LOW** | `profile` | Layer selection order is non-deterministic (HashMap) | Hardening |
| **[BUG-08](#bug-08-low--config-apply_to_run-cannot-disable-auto_profiles)** | **LOW** | `config` | Configuration OR logic prevents disabling `auto_profiles` | Hardening |

---

## Detailed Active Findings

### BUG-01 [CRITICAL] — Missing Root Directory `/` Grant in macOS System-Runtime
- **File:** [system-runtime.toml](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/profiles/macos/system-runtime.toml)
- **Description:** On macOS, every launched process inherits its current working directory (e.g. `/`) from `launchd` and must be able to resolve metadata for the root directory `/`. The macOS backend [macos.rs](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/macos.rs) documents that a `(literal "/")` grant is mandatory, yet neither [base.toml](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/profiles/base.toml) nor [system-runtime.toml](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/profiles/macos/system-runtime.toml) contains this grant.
- **Impact:** Any command executed under a standard profile stack on macOS crashes immediately with a `SIGABRT` (exit 134). This issue went unnoticed because the field-test harness manually injects the `/` grant in its test base, hiding the bug.
- **Fix:** Add `{ path = "/", access = "ro", match = "literal" }` to [system-runtime.toml](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/profiles/macos/system-runtime.toml).

### BUG-02 [CRITICAL] — Missing SBPL Scheme Macro Helpers for Safehouse Profiles
- **File:** [macos.rs](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/macos.rs)
- **Description:** Several built-in integration profiles ported from the macOS Safehouse project (such as [ssh.toml](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/profiles/integrations/ssh.toml) and [1password.toml](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/profiles/integrations/1password.toml)) use custom Scheme macro helpers like `home-literal`, `home-subpath`, and the variable `HOME_DIR` in their `raw` SBPL blocks. 
  Because `isol8`'s macOS backend concatenates raw SBPL blocks verbatim without defining these helpers, `sandbox-exec` rejects the generated policy as containing undefined symbols.
- **Impact:** Over 10 major integration profiles fail to compile on macOS, crashing the sandbox wrapper with exit code 65 (bubbles up as CLI exit 1).
- **Fix:** Update [macos.rs](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/macos.rs) to render Scheme macro definitions at the top of the Seatbelt policy header, mapping them to the resolved `$HOME` directory:
  ```lisp
  (define HOME_DIR "<resolved_home>")
  (define (home-literal path) (literal (string-append HOME_DIR path)))
  (define (home-subpath path) (subpath (string-append HOME_DIR path)))
  ```

### BUG-03 [HIGH] — Tilde Expansion Missing on `--home`
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

### BUG-04 [HIGH] — Invalid SBPL Syntax in `process-control.toml`
- **File:** [process-control.toml:13](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/profiles/integrations/process-control.toml#L13)
- **Description:** The raw SBPL block contains the line:
  ```text
  (kill, pkill, kill -0)
  ```
- **Impact:** This is invalid SBPL syntax. When a user runs a command on macOS that includes this profile, `sandbox-exec` fails to compile the policy and exits with code 65, crashing the wrapper process.
- **Fix:** Remove the invalid line or turn it into a Lisp-style comment prefixing with semicolons (`;; (kill, pkill, kill -0)`).

### SEC-01 [HIGH] — Scratch HOME Path is Predictable and Hijackable
- **File:** [home.rs:48](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/home.rs#L48)
- **Description:** Scratch homes are generated in the shared `/tmp` directory using the wrapper's process ID:
  ```rust
  let dir = std::env::temp_dir().join(format!("isol8-{}-home", std::process::id()));
  ```
- **Impact:** On shared multi-user systems, an attacker can predict the next process ID and pre-create the target directory or symlink, allowing them to intercept or hijack the sandboxed process's configuration/seeding directories.
- **Fix:** Introduce a cryptographically secure random sequence (e.g., via the `tempfile` crate or a random `u64` generator) and use `std::fs::create_dir` (which fails if the directory already exists).

### BUG-05 [MEDIUM] — `HomeReplace.enabled` Flag Ignored
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

### BUG-06 [MEDIUM] — `init_template` YAML Generator Outputs TOML
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

### SEC-02 [MEDIUM] — No Network Confinement by Default
- **Files:** [network.toml](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/profiles/network.toml), [macos.rs](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/macos.rs)
- **Description:** The default sandbox policies on macOS leave outbound network fully open. There is no implicit network deny.
- **Impact:** A sandboxed process can connect to any host on the internet and exfiltrate data, making process isolation less effective.
- **Fix:** State in the documentation that Phase 1 does not restrict networks. In Phase 3, default to `(deny network-outbound)` and require explicit opt-in network layers.

### BUG-07 [LOW] — Non-Deterministic Auto-Select Ordering
- **File:** [profile.rs:323-334](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/profile.rs#L323-L334)
- **Description:** Auto-selected profiles are gathered by iterating a `HashMap` registry.
- **Impact:** HashMap iteration order is randomized in Rust. If multiple auto-selected layers are matched, they can be merged in different orders, leading to non-deterministic behavior.
- **Fix:** Collect names into a sorted vector before resolving requires.

---

## Deferred Linux Considerations (Work-in-Progress)

> [!NOTE]
> The following findings focus on the Linux backend implementation. Since Linux support is currently ignored/deferred for this phase, these issues do not impact active macOS environments, but should be addressed when Linux development resumes.

### 1. Incomplete HOME Replacement (Namespaces Disabled)
- **File:** [linux.rs:275-301](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L275-L301)
- **Issue:** Mount namespace and user namespace setup helper functions are defined but commented out/unused. HOME replacement on Linux is only enforced via the `$HOME` environment variable, which can be bypassed via `getpwuid` or hardcoded paths.
- **Remediation:** Re-enable user and mount namespaces using `CommandExt::pre_exec` to enter namespaces safely in the child process.

### 2. Ancestor Read Leakage
- **File:** [linux.rs:180-210](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L180-L210)
- **Issue:** The backend grants `AccessFs::{ReadFile | ReadDir}` permissions to all parent folders of allowed paths. This leaks full read access to `/home/user` if a user grants access to `/home/user/workspace/project`.
- **Remediation:** Remove parent directory rules from the Landlock ruleset (they are not needed for traversal) or restrict them strictly to metadata-only access.

### 3. Silently Ignoring MatchKind Constraints
- **File:** [linux.rs:157-178](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L157-L178)
- **Issue:** `literal`, `prefix`, and `regex` path constraints are silently ignored on Linux.
- **Remediation:** Support `MatchKind::Literal` by opening the target file FD. Warn or fail if other unsupported matchers are used.

### 4. Landlock Path Existence Fragility
- **File:** [linux.rs:232-238](file:///Users/mdn/works/projects/agent-manager/workspace/isol8/src/backends/linux.rs#L232-L238)
- **Issue:** `apply_landlock` bails with `ENOENT` if any path in the merged policy does not exist on the user's system, crashing the wrapper process.
- **Remediation:** Skip adding Landlock rules for non-existent read paths instead of bailing.
