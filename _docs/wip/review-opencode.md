# isol8 Code Audit Report

**Date:** Sat Jun 20 2026
**Reviewer:** opencode (mimo-v2-5-free)
**Scope:** Full codebase review — safety, correctness, soundness
**Codebase state:** Phase 1 (macOS MVP working, Linux Landlock partial)

---

## Executive Summary

isol8 is a well-architected deny-by-default sandbox with a solid security foundation. The core design — profile-driven, deny-first merge, env sanitization, HOME replacement — is sound. The macOS Seatbelt backend correctly implements last-match-wins SBPL generation. The Linux backend correctly applies `PR_SET_NO_NEW_PRIVS` + Landlock.

However, several bugs, security gaps, and design issues need attention before the codebase is production-ready. The most critical issues are: (1) a broken YAML init template, (2) `--home` tilde not expanded, (3) `HomeReplace.enabled` field never checked, (4) no network confinement in the default policy, and (5) the Linux backend doesn't enforce HOME via bind-mount.

---

## Severity Legend

| Label | Meaning |
|-------|---------|
| **CRITICAL** | Security vulnerability or data-loss bug; must fix before any release |
| **HIGH** | Significant correctness/security issue; fix before production |
| **MEDIUM** | Bug or design gap; fix before wide adoption |
| **LOW** | Minor issue or hardening opportunity |
| **INFO** | Observation or documentation gap |

---

## 1. Bugs (Correctness)

### BUG-01 [MEDIUM] — `init_template` YAML output is actually TOML

**File:** `src/config.rs:173-185`

Both the `"yaml" | "yml"` branch and the default (TOML) branch produce identical TOML-formatted output. Running `isol8 @init --format yaml` creates an invalid YAML file.

```rust
// Both branches emit the same TOML string:
"yaml" | "yml" => Ok(format!(
    r#"default_profiles = {dp:?}"#  // TOML syntax, not YAML
)),
```

**Fix:** The YAML branch should emit YAML syntax (`default_profiles: [...]` with colon-separator and dash-list format).

---

### BUG-02 [MEDIUM] — `--home` value tilde is not expanded

**File:** `src/home.rs:43-44`

When a user passes `--home ~/scratch` or `ISOL8_HOME=~/scratch`, the tilde is never expanded. The effective HOME becomes the literal path `~/scratch`. All subsequent `~` expansion of path grants (in `load_merged`) produces broken paths like `~/scratch/.cargo`.

```rust
let path = if let Some(home) = run.home() {
    PathBuf::from(home)  // no expand_tilde()
```

**Fix:** Call `home::expand_tilde` on the `--home` value before using it.

---

### BUG-03 [MEDIUM] — `HomeReplace.enabled` field is never checked

**Files:** `src/profile.rs:107-118`, `src/home.rs:26-57`

The `HomeReplace` struct defines `enabled: bool`, but `home::resolve()` never reads it. The behavior is driven entirely by whether `path` is `Some` or `auto_scratch` is `true`. A profile author writing `home_replace = { enabled = false, auto_scratch = true }` would expect home replacement to be disabled, but `auto_scratch: true` still triggers scratch-home creation.

**Fix:** Add `if hr.enabled` check in `home::resolve()`, or remove the field if it's not intended.

---

### BUG-04 [MEDIUM] — `process-control.toml` contains invalid SBPL

**File:** `profiles/integrations/process-control.toml:13`

```
(kill, pkill, kill -0)
```

This is shell syntax, not valid SBPL. If this layer is included in a macOS sandbox policy, `sandbox-exec` rejects it with exit 65 (policy-compile error). The backend catches this, but the built-in profile is broken.

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

**Mitigation:** Profiles that use the blanket `MachLookup` capability (e.g., `macos/system-runtime.toml` grants specific services via raw SBPL instead). But some integration profiles do grant the blanket capability.

**Fix:** Consider splitting into specific vs. blanket mach-lookup variants. Document the risk.

---

### SEC-04 [MEDIUM] — `MatchKind::Regex` allows arbitrarily broad path matching

**File:** `src/profile.rs:37`, `src/backends/macos.rs:228`

Regex patterns like `.*` would grant access to the entire filesystem. A crafted regex with catastrophic backtracking (e.g., `(a+)+$`) could cause denial of service in the sandbox process.

**Mitigation:** Built-in profiles use specific regexes. User-authored profiles via `--profile-path` are the risk.

**Fix:** When `--show-policies` is used, highlight regex grants with a warning flag. Consider limiting regex complexity for user-supplied profiles.

---

### SEC-05 [MEDIUM] — Scratch home directory path is predictable

**File:** `src/home.rs:48`

```rust
let dir = std::env::temp_dir().join(format!("isol8-{}-home", std::process::id()));
```

On a shared system, an attacker who knows the PID could potentially create a symlink at the expected path before the directory is created.

**Mitigation:** `create_dir_all` follows symlinks, so a symlink attack would fail if the attacker creates a symlink (it would try to create dirs inside the target). But if the attacker creates a directory first, seed files go into the attacker's directory.

**Fix:** Use a random component (e.g., `tempfile::tempdir_in` or `random u64`). After `create_dir_all`, verify the path is a real directory via `fs::symlink_metadata`.

---

### SEC-06 [MEDIUM] — No `PR_SET_NO_NEW_PRIVS` on macOS

**Files:** `src/backends/macos.rs`, `src/backends/linux.rs:310-319`

The Linux backend correctly calls `set_no_new_privs()`. The macOS backend does not. While Seatbelt policy is inherited across `exec()`, `no_new_privs` would be an additional defense layer if a sandbox escape is found.

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

**File:** `src/home.rs:105-119`

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

### DES-01 [MEDIUM] — Linux HOME bind-mount not wired

**File:** `src/backends/linux.rs:275-301`

The `child_setup_and_exec` function receives `_effective_home` (prefixed with underscore, indicating unused) but never bind-mounts it over the real HOME. The `bind_mount_home()` function exists (line 361) but is `#[allow(dead_code)]` and never called.

This means on Linux, HOME replacement is only enforced via the environment variable, not via mount namespace isolation. A process that uses `getpwuid()` or hardcoded paths could bypass the HOME replacement.

**Fix:** Wire up user namespace + mount namespace + bind-mount when available. The dead code is ready — it needs integration.

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

**File:** `src/home.rs:43-44`

When `--home` is provided, the path is used directly without checking if it exists, is a directory, or is writable.

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

The following aspects are well-implemented and correct:

| Area | Details |
|------|---------|
| **Deny-by-default** | Both backends start with deny-by-default (macOS: `(deny default)`, Linux: Landlock implicit deny) |
| **Env sanitization** | Hardcoded 7-variable allowlist; `env_clear()` before `envs()` in both backends; secrets are dropped |
| **HOME resolution order** | `--home` > profile `home_replace.path` > auto-scratch > real home; resolved BEFORE any path computation |
| **~ expansion** | Only expands leading `~` or `~/...`; mid-string tilde not expanded (tested) |
| **Profile merge** | Deny-first merge with highest-wins per `(path, match)` key; env first-writer-wins |
| **Cycle detection** | DFS topo-sort with gray/black states; errors with the cycle path |
| **serde(deny_unknown_fields)** | Consistently applied on all profile and config structs |
| **macOS symlink resolution** | Both `/tmp` and `/private/tmp` forms emitted for each grant |
| **Last-match-wins ordering** | Ancestor metadata → allows → none denies → capabilities → raw passthrough |
| **`none` deny uses `file-read* file-write*`** | Not bare `file*` which doesn't block writes (verified against real sandbox-exec) |
| **Landlock deny-by-default** | `Access::None` simply omits the rule; no rule = no access |
| **PR_SET_NO_NEW_PRIVS** | Called first in child setup before Landlock rules |
| **Exit code 65 handling** | Policy-compile errors surfaced with the full generated policy |
| **Field tests** | 8 scenarios covering read denial, write, seed, env, and network |
| **Profile validation** | All ~70 built-in profiles parse correctly |

---

## 6. Recommendations (Priority Order)

### Must Fix (Before Any Release)

1. **BUG-02** — Expand tilde in `--home` value
2. **BUG-03** — Check `HomeReplace.enabled` in `home::resolve()`
3. **BUG-04** — Fix `process-control.toml` invalid SBPL
4. **BUG-01** — Fix YAML init template to emit actual YAML
5. **SEC-01** — Document prominently that network is not confined

### Should Fix (Before Production)

6. **DES-01** — Wire up Linux HOME bind-mount
7. **SEC-02** — Warn when `--profile-path` is used (raw SBPL trust)
8. **SEC-05** — Use random component in scratch home name
9. **ERR-01** — Catch sandbox-exec exit codes 64 and 71
10. **BUG-05** — Make auto-selected layer order deterministic

### Nice to Have

11. **SEC-03** — Split MachLookup into specific/blanket variants
12. **SEC-04** — Warn on regex grants in `--show-policies`
13. **SEC-06** — Add `PR_SET_NO_NEW_PRIVS` on macOS
14. **SEC-12** — Default cloud credentials to `ro`
15. **SEC-13** — Make SSH agent access opt-in
16. **DES-03** — Canonicalize paths during merge
17. **DES-04** — Separate `MergedProfile` type

---

## 7. Dependency Audit

| Crate | Version | Risk |
|-------|---------|------|
| `clap` | 4 | LOW — widely used, well maintained |
| `serde` | 1 | LOW — standard |
| `toml` | 0.8 | LOW — standard |
| `serde_yaml` | 0.9 | LOW — standard |
| `anyhow` | 1 | LOW — standard |
| `landlock` | 0.4 | LOW — Linux-only, maintained by the Landlock developers |
| `nix` | 0.31 | LOW — Linux-only, widely used |
| `enumflags2` | 0.7 | LOW — Linux-only, used with landlock |

No known vulnerabilities. All dependencies are widely used and actively maintained. The dependency tree is minimal (no transitive bloat).

---

## 8. Test Coverage Assessment

| Module | Unit Tests | Integration Tests | Field Tests |
|--------|-----------|-------------------|-------------|
| `profile.rs` | 13 tests | 8 (profile_merge.rs), 1 (profile_path.rs), 12 (profile_filters.rs) | — |
| `env.rs` | 3 tests | — | Scenario 6-7 |
| `home.rs` | 4 tests | — | Scenario 3-5 |
| `config.rs` | 2 tests | — | — |
| `filter.rs` | 5 tests | — | — |
| `backends/macos.rs` | 9 tests | — | Scenarios 1-5 |
| `backends/linux.rs` | 4 tests | — | — |

**Strengths:** Good coverage of the merge logic, filter matching, and SBPL generation. Field tests prove real OS enforcement.

**Gaps:**
- No tests for `--home` tilde expansion (BUG-02)
- No tests for `HomeReplace.enabled` (BUG-03)
- No tests for `init_template` YAML output (BUG-01)
- No tests for non-deterministic auto-select ordering (BUG-05)
- No tests for error paths in sandbox-exec invocation
- No tests for the Linux backend's `child_setup_and_exec` path

---

## 9. Conclusion

isol8 has a strong architectural foundation. The deny-by-default model, profile-driven design, and deny-first merge are well-implemented. The macOS Seatbelt backend is production-quality for the MVP.

The main risks are: (1) several correctness bugs in the HOME replacement pipeline, (2) no network confinement, (3) the Linux backend's incomplete HOME isolation, and (4) the unconstrained raw SBPL passthrough in user-supplied profiles.

None of these are surprising for a Phase 1 MVP. The bugs are fixable without architectural changes. The security gaps are documented (R5 roadmap). The code quality is high — clean error handling, good test coverage, no panics on user input, `serde(deny_unknown_fields)` throughout.

**Overall assessment: Sound architecture with known gaps. Fix the critical/high items before any release.**
