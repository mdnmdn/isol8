# isol8 Code Audit Report

**Date:** Sat Jun 20 2026
**Reviewer:** opencode (mimo-v2-5-free)
**Scope:** macOS Seatbelt backend, profile system, env/home isolation (Linux deferred)
**Codebase state:** Phase 1 macOS MVP working; Linux backend deferred

---

## Executive Summary

isol8 is a well-architected deny-by-default sandbox with a solid security foundation. The core design — profile-driven, deny-first merge, env sanitization, HOME replacement — is sound. The macOS Seatbelt backend correctly implements last-match-wins SBPL generation.

The three most critical correctness bugs (YAML init template, `--home` tilde expansion, `HomeReplace.enabled` check) have been fixed. The scratch home predictability issue has also been addressed with a unique-name strategy. What remains are security design gaps (no network confinement, unconstrained raw SBPL passthrough, broad mach-lookup capability), a broken built-in profile, and minor hardening opportunities.

Linux-specific code (Landlock, namespace helpers, `PR_SET_NO_NEW_PRIVS`, bind-mount) is explicitly out of scope for this review.

---

## Severity Legend

| Label | Meaning |
|-------|---------|
| **CRITICAL** | Security vulnerability or data-loss bug; must fix before any release |
| **HIGH** | Significant correctness/security issue; fix before production |
| **MEDIUM** | Bug or design gap; fix before wide adoption |
| **LOW** | Minor issue or hardening opportunity |
| **INFO** | Observation or documentation gap |
| **FIXED** | Previously identified, now resolved in codebase |
| **OOS** | Out of scope (Linux backend, deferred) |

---

## 1. Bugs (Correctness)

### BUG-01 [FIXED] — `init_template` YAML output was TOML

**File:** `src/config.rs:188-226`

Previously both branches emitted identical TOML. Now fixed: the YAML branch (line 192) generates proper YAML with `default_profiles:` (colon separator), `- item` list syntax, and YAML-style comments. The TOML branch (line 213) correctly uses `=` and `[...]`.

---

### BUG-02 [FIXED] — `--home` value tilde was not expanded

**File:** `src/home.rs:49-50`

Previously `PathBuf::from(home)` was used without expansion. Now fixed: `expand_tilde(home, &real)` is called (line 50). A dedicated test (`resolve_expands_tilde_in_cli_home`, line 183) verifies `~/scratch` resolves against the real home.

---

### BUG-03 [FIXED] — `HomeReplace.enabled` was never checked

**File:** `src/home.rs:35-37`

Previously the `enabled` field on `HomeReplace` was ignored. Now fixed: `if !hr.enabled { continue; }` skips disabled home_replace layers. A dedicated test (`resolve_honors_home_replace_enabled_false`, line 204) verifies the behavior.

---

### BUG-04 [MEDIUM] — `process-control.toml` contains invalid SBPL

**File:** `profiles/integrations/process-control.toml:13`

```
(kill, pkill, kill -0)
```

This is shell syntax, not valid SBPL. If this layer is included in a macOS sandbox policy, `sandbox-exec` rejects it with exit 65 (policy-compile error). The backend catches this, but the built-in profile is broken and cannot be used.

**Fix:** Either remove the line or replace with a proper SBPL comment (`;; kill, pkill, kill -0`).

---

### BUG-05 [LOW] — Auto-selected layer order is non-deterministic

**File:** `src/profile.rs:322-333`

Auto-selected layers iterate over `registry.entries` (a `HashMap`), which has randomized iteration order. When multiple agent layers match, the effective policy could vary between runs. The `resolve_requires` DFS applies topological ordering, but the selection order affects tie-breaking.

**Fix:** Collect auto-selected names into a `BTreeSet` or sort before feeding to `resolve_requires`.

---

### BUG-06 [LOW] — Config `apply_to_run` cannot disable `auto_profiles`

**File:** `src/config.rs:109`

```rust
run.opts.auto_profiles = run.opts.auto_profiles || cfg.auto_profiles;
```

Uses OR semantics: a config file setting `auto_profiles = false` cannot override a CLI `--auto-profiles` flag. There is also no `ISOL8_AUTO_PROFILES` env override (unlike all other config knobs).

**Fix:** Implement clean override precedence (CLI > env > config), or document the OR behavior.

---

## 2. Security Concerns

### SEC-01 [HIGH] — No network confinement in default policy

**Files:** `profiles/network.toml`, `src/backends/macos.rs`

The default macOS sandbox allows unrestricted outbound network access. There is no `(deny network-outbound)` in the generated SBPL. The `network.toml` profile is a stub (requires only `base` + `macos/system-runtime`). The field test scenario 08 is `SKIP`.

An AI agent running under isol8 can make arbitrary outbound connections, exfiltrate data, or download malware.

**Mitigation:** This is documented as not-yet-implemented (R5 deferred to Phase 3). However, users may not realize this.

**Fix:** At minimum, document prominently in README. Better: emit `(deny network-outbound)` in the default policy and make network allowlisting explicit.

---

### SEC-02 [MEDIUM] — Raw SBPL passthrough is unvalidated

**File:** `src/backends/macos.rs:202-209`

The `macos.raw` field is concatenated verbatim into the SBPL policy string. There is no syntax validation before passing it to `sandbox-exec`. A syntactically valid but semantically dangerous raw rule (e.g., `(allow network-outbound (remote internet))`) could widen confinement beyond what the profile author intended.

This is a trust boundary issue: users loading third-party profile-path TOMLs are implicitly trusting all raw SBPL in those files.

**Mitigation:** Profiles are embedded at compile time or loaded from `--profile-path`. `#[serde(deny_unknown_fields)]` prevents unknown TOML keys.

**Fix:** When `--profile-path` is used, print a warning that user-supplied profiles can grant arbitrary access. Consider a `--untrusted-profile` mode that strips or rejects raw SBPL.

---

### SEC-03 [MEDIUM] — `Capability::MachLookup` grants access to ALL mach services

**File:** `src/backends/macos.rs:250`

```rust
Capability::MachLookup => "(allow mach-lookup)",
```

Without a `(global-name ...)` filter, the confined process can talk to any Mach service on the system (keychain, network config, privileged services).

**Mitigation:** The `macos/system-runtime.toml` grants specific services via raw SBPL instead of the blanket capability. But integration profiles that use `Capability::MachLookup` directly grant broad access.

**Fix:** Consider splitting into specific vs. blanket mach-lookup variants. Document the risk.

---

### SEC-04 [MEDIUM] — `MatchKind::Regex` allows arbitrarily broad path matching

**File:** `src/profile.rs:37`, `src/backends/macos.rs:228`

Regex patterns like `.*` would grant access to the entire filesystem. A crafted regex with catastrophic backtracking (e.g., `(a+)+$`) could cause denial of service in the sandbox process.

**Mitigation:** Built-in profiles use specific regexes. User-authored profiles via `--profile-path` are the risk.

**Fix:** When `--show-policies` is used, highlight regex grants with a warning flag. Consider limiting regex complexity for user-supplied profiles.

---

### SEC-05 [FIXED] — Scratch home directory path was predictable

**File:** `src/home.rs:63-94`

Previously used PID-only naming (`isol8-{pid}-home`). Now fixed: `create_scratch_home()` uses PID + nanosecond timestamp + atomic counter for uniqueness, retries up to 16 times, and verifies the created path is not a symlink via `symlink_metadata`. A test (`scratch_home_paths_are_unique`, line 228) verifies two consecutive calls produce different paths.

---

### SEC-06 [MEDIUM] — No `PR_SET_NO_NEW_PRIVS` on macOS

**File:** `src/backends/macos.rs`

The Linux backend correctly calls `set_no_new_privs()` (out of scope, but noted as reference). The macOS backend does not. While Seatbelt policy is inherited across `exec()`, `no_new_privs` would be an additional defense layer if a sandbox escape is found.

**Fix:** Consider using `proc_set_no_new_privs` (available via `nix` crate on macOS) as hardening.

---

### SEC-07 [MEDIUM] — `(allow system-socket)` in macOS system-runtime

**File:** `profiles/macos/system-runtime.toml:115`

```
(allow system-socket)
```

This is a broad network permission in the default macOS profile stack. It allows access to all system sockets. Necessary for basic network resolution but broader than ideal.

**Fix:** Document this explicitly. Consider narrowing to specific system sockets.

---

### SEC-08 [LOW] — Ancestor metadata grants leak directory structure

**File:** `src/backends/macos.rs:140-158`

Every granted path gets `file-read-metadata` on ALL ancestors (including `/`). This leaks directory existence and metadata (timestamps, permissions) via `stat()`, even without read access to contents.

**Mitigation:** Inherent to how macOS path resolution works. Documented in the code comments.

---

### SEC-09 [LOW] — `sbpl_string` doesn't escape newlines

**File:** `src/backends/macos.rs:314-325`

The escape function handles `\` and `"` but not newlines. A path grant containing a newline would break out of the SBPL double-quoted string.

**Mitigation:** Filesystem paths rarely contain newlines. Low risk.

---

### SEC-10 [LOW] — Higher layers can override a lower layer's `none` deny

**File:** `src/profile.rs:429-434`

The merge function uses highest-layer-wins for each `(path, match)` key. A higher layer can re-grant access that a lower layer denied. This is documented behavior but could surprise profile authors.

**Mitigation:** The `--show-policies` output reveals the effective policy. Users can verify.

---

### SEC-11 [LOW] — Seed files follow symlinks silently

**File:** `src/home.rs:140-156`

`copy_readonly` uses `symlink_metadata` to check the source type, but `std::fs::copy` follows symlinks. A symlinked seed entry could exfiltrate data from outside the real home.

**Mitigation:** The seed list is controlled by the profile author, not the user at runtime.

---

### SEC-12 [LOW] — Cloud credentials granted `rw` by default

**File:** `profiles/integrations/cloud-credentials.toml:8-11`

Grants `rw` access to `~/.aws`, `~/.config/gcloud`, `~/.azure`. This allows a confined process to read AND modify credential files.

**Fix:** Consider making cloud credential access `ro` by default, with `rw` only when explicitly needed.

---

### SEC-13 [LOW] — SSH agent access is allowed by default

**File:** `profiles/integrations/ssh.toml:21,30-34`

Grants `rw` to `~/.ssh/agent` and `network-outbound` to SSH agent sockets. The `ssh-agent-default-deny.toml` profile exists but must be explicitly layered on.

**Fix:** Consider making SSH agent access opt-in (require explicit `--profile ssh`) rather than opt-out.

---

## 3. Design Issues

### DES-01 [OOS] — Linux HOME bind-mount not wired

**File:** `src/backends/linux.rs:275-301` (out of scope)

The `child_setup_and_exec` function receives `_effective_home` but never bind-mounts it. The `bind_mount_home()` function exists but is `#[allow(dead_code)]`. This means on Linux, HOME replacement is only enforced via the environment variable.

**Status:** Out of scope — Linux backend deferred.

---

### DES-02 [MEDIUM] — `home_replace` merge is asymmetric

**File:** `src/profile.rs:438-445`

The highest layer's `home_replace` completely replaces the struct (including `enabled`, `auto_scratch`, `path`), but seeds are unioned across all layers. A lower layer that sets `auto_scratch: true` will have its seeds included, but its `auto_scratch: true` is overridden if the higher layer sets `auto_scratch: false`.

**Fix:** Document this asymmetry clearly, or change to a fully-union model.

---

### DES-03 [LOW] — No path normalization in `PathGrant.path`

**File:** `src/profile.rs:42-47`

Paths are stored as raw strings. `/home/user/../etc/passwd` and `/etc/passwd` would be stored as different keys in the merge map. While the OS-level enforcement resolves paths canonically, the merge logic operates on string keys.

**Fix:** Canonicalize paths during merge, or document that non-canonical paths may cause inconsistencies.

---

### DES-04 [LOW] — Merged profile is partially valid

**File:** `src/profile.rs:482-490`

The merged profile has empty `requires`, `filter`, and `policies`. Code that receives a merged profile and tries to iterate `policies` or follow `requires` would get empty results silently.

**Fix:** Consider a separate `MergedProfile` type, or document that merged profiles have empty metadata fields.

---

### DES-05 [INFO] — `read_dir` errors silently dropped

**File:** `src/profile.rs:267`

```rust
for entry in std::fs::read_dir(dir)?.flatten() {
```

`.flatten()` silently skips `Err` entries from `read_dir`. While acceptable for optional user config directories, it could hide real issues when loading `--profile-path` directories.

---

### DES-06 [INFO] — `--home` path not validated for existence

**File:** `src/home.rs:49-50`

When `--home` is provided, the path is used directly after tilde expansion without checking if it exists, is a directory, or is writable.

---

### DES-07 [INFO] — No profile override warning

**File:** `src/profile.rs:289-296`

When a user-provided profile-path file has the same name as a builtin, the user version silently replaces the builtin. No warning is logged.

---

### DES-08 [INFO] — `resolve_symlinks` falls back to input on failure

**File:** `src/backends/macos.rs:291`

If nothing along the path can be canonicalized, the original path is returned. Only one form is emitted, which could miss the resolved form. In practice, this is unlikely for system paths.

---

## 4. Error Handling

### ERR-01 [LOW] — Only exit code 65 is caught from sandbox-exec

**File:** `src/backends/macos.rs:83-90`

Exit codes 64 (usage error) and 71 (exec failure) are not specially handled. The command's exit code is returned to the caller, but there is no diagnostic message explaining that sandbox-exec itself failed.

**Fix:** Catch exit codes 64 and 71 and emit a warning to stderr.

---

### ERR-02 [LOW] — `process::exit` bypasses Drop

**File:** `src/main.rs:78`

```rust
std::process::exit(code);
```

This bypasses destructors and `Drop` implementations. The temp home directory is NOT cleaned up on exit. The field test cleans up unless `--keep` is passed, but the main binary does not clean scratch homes.

**Fix:** Use `std::process::exit` only when necessary, or register a cleanup handler.

---

### ERR-03 [INFO] — `unwrap()` in build.rs

**File:** `build.rs:6,8,29`

Uses `unwrap()` for `CARGO_MANIFEST_DIR`, `OUT_DIR`, and `fs::write`. Acceptable — these are guaranteed by Cargo in build script context.

---

## 5. Positive Findings

The following aspects are well-implemented and correct (macOS-focused):

| Area | Details |
|------|---------|
| **Deny-by-default** | macOS: `(deny default)` always first in generated SBPL |
| **Env sanitization** | Hardcoded 7-variable allowlist; `env_clear()` before `envs()`; secrets dropped |
| **HOME resolution order** | `--home` > profile `home_replace.path` > auto-scratch > real home; resolved BEFORE path computation |
| **~ expansion** | Only expands leading `~` or `~/...`; mid-string tilde not expanded; tilde in `--home` now expanded (tested) |
| **`HomeReplace.enabled`** | Now checked in `home::resolve()`; disabled layers skipped (tested) |
| **Scratch home uniqueness** | Uses PID + timestamp + atomic counter; symlink check; retry loop (tested) |
| **Profile merge** | Deny-first merge with highest-wins per `(path, match)` key; env first-writer-wins |
| **Cycle detection** | DFS topo-sort with gray/black states; errors with the cycle path |
| **serde(deny_unknown_fields)** | Consistently applied on all profile and config structs |
| **macOS symlink resolution** | Both `/tmp` and `/private/tmp` forms emitted for each grant |
| **Last-match-wins ordering** | Ancestor metadata → allows → none denies → capabilities → raw passthrough |
| **`none` deny uses `file-read* file-write*`** | Not bare `file*` which doesn't block writes (verified against real sandbox-exec) |
| **Exit code 65 handling** | Policy-compile errors surfaced with the full generated policy |
| **Field tests** | 8 scenarios covering read denial, write, seed, env, and network |
| **Profile validation** | All ~70 built-in profiles parse correctly |

---

## 6. Recommendations (Priority Order)

### Must Fix (Before Any Release)

1. **BUG-04** — Fix `process-control.toml` invalid SBPL
2. **SEC-01** — Document prominently that network is not confined

### Should Fix (Before Production)

3. **SEC-02** — Warn when `--profile-path` is used (raw SBPL trust)
4. **SEC-06** — Add `PR_SET_NO_NEW_PRIVS` on macOS
5. **ERR-01** — Catch sandbox-exec exit codes 64 and 71
6. **BUG-05** — Make auto-selected layer order deterministic

### Nice to Have

7. **SEC-03** — Split MachLookup into specific/blanket variants
8. **SEC-04** — Warn on regex grants in `--show-policies`
9. **SEC-07** — Narrow `(allow system-socket)` in system-runtime
10. **SEC-12** — Default cloud credentials to `ro`
11. **SEC-13** — Make SSH agent access opt-in
12. **DES-02** — Document or fix asymmetric home_replace merge
13. **DES-03** — Canonicalize paths during merge
14. **DES-04** — Separate `MergedProfile` type

### Out of Scope (Linux, Deferred)

- **DES-01** — Wire up Linux HOME bind-mount (requires user namespace + mount namespace)
- All Landlock/namespace-related findings in `src/backends/linux.rs`

---

## 7. Dependency Audit

| Crate | Version | Risk |
|-------|---------|------|
| `clap` | 4 | LOW — widely used, well maintained |
| `serde` | 1 | LOW — standard |
| `toml` | 0.8 | LOW — standard |
| `serde_yaml` | 0.9 | LOW — standard |
| `anyhow` | 1 | LOW — standard |
| `landlock` | 0.4 | OOS — Linux-only |
| `nix` | 0.31 | OOS — Linux-only |
| `enumflags2` | 0.7 | OOS — Linux-only |

No known vulnerabilities. All macOS-relevant dependencies are minimal and well maintained.

---

## 8. Test Coverage Assessment

| Module | Unit Tests | Integration Tests | Field Tests |
|--------|-----------|-------------------|-------------|
| `profile.rs` | 13 tests | 8 (profile_merge.rs), 1 (profile_path.rs), 12 (profile_filters.rs) | — |
| `env.rs` | 3 tests | — | Scenario 6-7 |
| `home.rs` | 7 tests | — | Scenario 3-5 |
| `config.rs` | 2 tests | — | — |
| `filter.rs` | 5 tests | — | — |
| `backends/macos.rs` | 9 tests | — | Scenarios 1-5 |

**Strengths:** Good coverage of the merge logic, filter matching, and SBPL generation. Field tests prove real OS enforcement. New tests for tilde expansion, `HomeReplace.enabled`, and scratch home uniqueness.

**Gaps (macOS scope):**
- No tests for `init_template` YAML output format
- No tests for non-deterministic auto-select ordering (BUG-05)
- No tests for error paths in sandbox-exec invocation
- No tests for `sbpl_string` newline edge case

---

## 9. Conclusion

isol8 has a strong architectural foundation. The deny-by-default model, profile-driven design, and deny-first merge are well-implemented. The macOS Seatbelt backend is production-quality for the MVP.

Three correctness bugs (YAML init, `--home` tilde, `HomeReplace.enabled`) and the scratch home predictability issue have been fixed since the initial review. What remains is primarily security design work: (1) no network confinement, (2) unconstrained raw SBPL passthrough in user-supplied profiles, (3) broad mach-lookup capability, and (4) a broken built-in profile (`process-control.toml`).

None of these are surprising for a Phase 1 MVP. The security gaps are documented (R5 roadmap). The code quality is high — clean error handling, good test coverage, no panics on user input, `serde(deny_unknown_fields)` throughout.

**Overall assessment: Sound architecture with known gaps. Fix the two must-fix items (BUG-04, SEC-01) before any release.**
