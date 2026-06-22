# isol8 — Linux Backend Support

Status of the Linux (Landlock) sandbox backend.

---

## Overview

The Linux backend uses the **Landlock LSM** (Linux Security Module) to enforce
deny-by-default filesystem access control on confined processes. It runs
unprivileged — no root, no capabilities, no daemons. Combined with
`PR_SET_NO_NEW_PRIVS` (prevents setuid/setgid escalation) and a sanitized
environment, it provides the same isolation model as the macOS Seatbelt backend,
adapted to Linux's security primitives.

**Verified on:** WSL2 (kernel 5.15, `CONFIG_SECURITY_LANDLOCK=y`), OrbStack VM
(kernel 7.0, Landlock enabled). Expected to work on any Linux kernel ≥ 5.13
with Landlock compiled in.

---

## Features

| Feature | Status | Mechanism |
|---------|--------|-----------|
| Filesystem deny-by-default | Working | Landlock `PathBeneath` per-path rules |
| Per-path `ro` / `rw` / `none` | Working | `AccessFs::{ReadFile, ReadDir, Execute, WriteFile, Make*}` |
| `metadata` access | Working | Maps to `ReadFile \| ReadDir \| Execute` (ro + exec; see limitations) |
| `PR_SET_NO_NEW_PRIVS` | Working | `prctl()` before Landlock + exec |
| Environment sanitization | Working | Allowlist + `--env-pass` + `--set-env` (shared with macOS) |
| HOME replacement (env-level) | Working | `HOME` set in env; `~` expanded against replacement |
| Profile composability | Working | Layered TOML, deny-first merge, `requires` inheritance |
| `--show-policies` / `--dry-run` | Working | Prints Landlock rules + env + command |
| Auto-profile selection | Working | Executable filter matching (shared with macOS) |
| `--add-dirs-rw` / `--add-dirs-ro` | Working | CLI grants folded into profile before merge |
| `confine_executable` | Working | Resolves `cmd[0]` to absolute path, auto-grants `ro` |
| Landlock ABI probe | Working | Direct VERSION syscall probe (no side-effect `restrict_self`); reports real ABI on `--dry-run` |
| Field tests | Working | Scenarios 1–9 (cross-platform) + 10–16 (Linux-specific) |

---

## Architecture

### Execution flow

```
isol8 [OPTIONS] <COMMAND>
  │
  ├─ resolve::effective_policy()     ← profile merge, HOME, env, rewrite
  ├─ resolve::confine_executable()   ← resolve cmd[0], auto-grant ro
  │
  └─ backends::linux::LinuxBackend::spawn()
       │
       ├─ build_landlock_rules(profile)  ← PathGrant → LandlockRule (fd + rights)
       │
       ├─ fork()
       │    │
       │    ├─ Parent → waitpid() → exit code
       │    │
       │    └─ Child:
       │         1. set_no_new_privs()
       │         2. apply_landlock(rules)    ← Ruleset + PathBeneath + restrict_self()
       │         3. execvp(command, env)     ← replaces process
       │
       └─ (namespace helpers — implemented but not wired in; see §Limitations)
```

### Key design decisions

**No ancestor rules.** Unlike macOS Seatbelt, Landlock's `PathBeneath` grants
access to the *entire subtree* beneath a directory. Adding ancestor rules for
path resolution (R2.3) inadvertently grants access to sibling directories —
e.g., adding `/home` as ancestor of `/home/user/.config` would grant read
access to all of `/home/`. Instead, Unix DAC handles path traversal: the child
process can traverse parent directories to reach granted paths. Landlock only
restricts which directory FDs can be opened.

**`subpath` match only.** Landlock's `PathBeneath` grants a subtree, so
`literal` (exact-match), `prefix`, and `regex` match kinds cannot be
faithfully represented. Only `MatchKind::Subpath` grants are emitted;
other match kinds are silently skipped.

**ABI probing.** `probe_landlock_abi()` issues a `landlock_create_ruleset` with
the `LANDLOCK_CREATE_RULESET_VERSION` flag (no ruleset is installed and
`restrict_self()` is never called). This reports the true kernel ABI in
`--dry-run` output (e.g. `v1 (enforced)`) with no side effects on the calling
process and no stacking against the 16-layer limit.

---

## Built-in Linux profiles

| Profile | Description |
|---------|-------------|
| `base` | Cross-platform minimal: `/usr` ro, `/bin` ro, `/tmp` rw, minimal `PATH` |
| `linux/system-runtime` | Linux essentials: dynamic linker paths (`/lib`, `/lib64`, `/usr/lib`, `/usr/lib64`), `/opt`, `/etc`, `/dev`, `/proc` ro; `/var/tmp`, `/var/cache`, `/run/user` rw; XDG metadata for `~/.config`, `~/.cache`, `~/.local` |
| `linux-system` | Backward-compat alias → requires `linux/system-runtime` |
| `linux/gui` | X11/Wayland: `/tmp/.X11-unix`, `/dev/dri`, fontconfig trees, `~/.cache/fontconfig` rw |
| `linux/secret-service` | GNOME Keyring/libsecret: `~/.local/share/keyrings` rw |

Default stack: `base` + `linux/system-runtime` (loaded via config
`default_profiles` or auto-detection). Additional layers added via
`--profile`, `auto_profiles`, or `--enable`.

---

## Known limitations

### 1. No stat-only (metadata) enforcement

Landlock has no `AccessFs::Metadata` right. The `metadata` access level in
profiles maps to `ReadFile | ReadDir | Execute` (same rights as `ro`, plus
execute for binaries under the subtree). True stat-only access (R2.3) is not
expressible in Landlock. `--dry-run` therefore labels such grants `META→ro` so
the effective policy is reported honestly.

### 2. No `literal` / `prefix` / `regex` matchers

Landlock's `PathBeneath` grants an entire subtree. A `literal` grant
(exact-match only, no children) cannot be represented. `prefix` and `regex`
match kinds are also unsupported. These match kinds are silently skipped by
`build_landlock_rules()`. If a profile relies on `literal` for fine-grained
access, it will not be enforced on Linux.

### 3. No ancestor metadata rules

On macOS, Seatbelt can grant stat-only access on parent directories for path
resolution (R2.3). Landlock cannot: `PathBeneath` on a parent grants read
access to the entire subtree, which would expose sibling directories. Instead,
Unix DAC handles path traversal — the kernel allows traversing directories the
user has execute permission on. This means:

- Paths that Unix DAC denies traversal to (e.g., mode `0700` dirs owned by
  another user) cannot be reached even if a Landlock rule covers them.
- Tools that need to `stat()` ancestors of granted paths may fail if those
  ancestors are not in the granted set.

### 4. HOME bind-mounting disabled

The functions `unshare_user_and_mount_ns()`, `write_uid_gid_mappings()`, and
`bind_mount_home()` are implemented in `src/backends/linux.rs` but **not wired
into** `child_setup_and_exec()`. When enabled, they would:

1. Enter a user namespace (`CLONE_NEWUSER`) to remap UIDs without root.
2. Enter a mount namespace (`CLONE_NEWNS`) for bind-mount operations.
3. Bind-mount the replacement `$HOME` over the real home path.

This provides robust R4.6 isolation — even `getpwuid()`-derived `~` resolves
into the replacement directory.

**Why disabled:** Some VM environments (notably OrbStack) block writes to
`/proc/self/uid_map` after `unshare(CLONE_NEWUSER)`. WSL2 allows it. The
helpers can be re-enabled when user namespace availability is confirmed at
runtime (detect before attempting `uid_map` write).

**Consequence without bind-mount:** The replacement `$HOME` is only set via the
environment variable. Tools that resolve home via `getpwuid()` (rare) or
`/etc/passwd` (very rare) will see the real home path, not the replacement.

### 5. No network isolation (R5)

Network access is **open** by default. The N0–N3 tiered isolation model
(cooperative proxy → rootless enforced → rooted enforced) is unimplemented.
A confined process can reach any host. See `_docs/project-description.md` §R5
for the full design.

### 6. No resource limits (R1.3)

CPU, memory, and PID count limits are not wired. The child process inherits
the parent's resource limits. cgroups v2 or `setrlimit` integration is
planned for a later phase.

### 7. `/proc` is fully readable

`/proc` is granted `ro` by `linux/system-runtime` for process introspection
(tools like `ps`, `top`, language runtime procfs reads). It cannot be
restricted granularly — Landlock rules apply to the entire `/proc` subtree.

### 8. WSL2 `/mnt/c` caveat

Windows drives mounted under `/mnt/c` (via 9P/drvfs) get weaker Landlock
guarantees. The 9P filesystem layer may not honor Landlock restrictions the
same way native Linux filesystems do. Keep confined work on the Linux
filesystem (e.g., `/home`, `/tmp`) for strongest enforcement.

---

## Field tests

Linux-specific scenarios in `src/bin/isol8-field-test.rs`:

| # | Scenario | What it proves |
|---|----------|---------------|
| 10 | `linux-deny-ungranted-path` | Landlock denies read on path outside any grant |
| 11 | `linux-rw-write-allowed` | `rw` grant allows file creation |
| 12 | `linux-ro-write-denied` | `ro` grant prevents writes |
| 13 | `linux-ro-read-allowed` | `ro` grant allows reads |
| 14 | `linux-real-home-denied` | Real home not exposed (no ancestor over-granting) |
| 15 | `linux-env-secret-absent` | Non-allowlisted env var stripped |
| 16 | `linux-env-path-home-present` | `HOME` and `PATH` present in env |

Cross-platform scenarios 1–9 also enforce on Linux (deny-by-default, rw/ro
tests, env allowlist, command rewrite).

Run: `cargo build --bin isol8-field-test && ./target/debug/isol8-field-test`

---

## Verified environments

| Environment | Kernel | Landlock | Enforcement | Notes |
|-------------|--------|----------|-------------|-------|
| WSL2 (Ubuntu 24.04) | 5.15.133.1-microsoft-standard-WSL2 | `CONFIG_SECURITY_LANDLOCK=y` | Fully enforced | Primary test target |
| OrbStack VM (Ubuntu 24.04) | 7.0.11-orbstack | `CONFIG_SECURITY_LANDLOCK=y` | Fully enforced | User namespaces blocked (`uid_map` write fails) |
| Bare metal / KVM | ≥ 5.13 | `CONFIG_SECURITY_LANDLOCK=y` | Expected enforced | Not tested in CI |

---

## Dependencies

```toml
[target.'cfg(target_os = "linux")'.dependencies]
landlock = "0.4"           # Safe Rust bindings for Landlock LSM
nix = { version = "0.31", features = ["sched", "mount", "fs", "user"] }
enumflags2 = "0.7"         # BitFlags for AccessFs
```

---

## Future work

- **Re-enable namespace helpers** — Detect `uid_map` availability at runtime;
  when supported, enter user + mount namespaces for HOME bind-mounting (R4.6).
- **Network tiers (R5)** — N1 cooperative proxy → N2 rootless enforced
  (pasta) → N3 rooted enforced (netns + nftables).
- **Resource limits (R1.3)** — cgroups v2 or `setrlimit` for CPU/mem/PID.
- **Seccomp profiles (Phase 4)** — Syscall filtering integrated with Landlock.
- **Pure-Landlock vs hybrid mode** — User-selectable: pure Landlock
  (simpler, no ns) vs full namespace isolation (stronger HOME isolation).
