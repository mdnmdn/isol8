# Linux Backend — Current State & Known Issues

**Date:** 2026-06-22  
**Status:** Landlock enforced on WSL2 (kernel 5.15). Ancestor over-granting bug fixed. Field tests enabled.

---

## What's implemented

### File: `src/backends/linux.rs`

- **Landlock ruleset builder** — converts `Profile.paths` into `BitFlags<AccessFs>` rules (deny-by-default).
- **`render_policy()`** — prints human-readable Landlock rules for `--dry-run`.
- **`spawn()`** — forks a child, applies `PR_SET_NO_NEW_PRIVS` + Landlock, then `execvp` the target.
- **Match-kind filter** — skips `literal`/`prefix`/`regex` grants (macOS Seatbelt matchers with no Landlock equivalent); only `subpath` grants are enforced.
- **No ancestor rules** — Landlock's `PathBeneath` grants subtree access, so ancestor rules would over-grant. Unix DAC handles path traversal.
- **ABI probe** — `probe_landlock_abi()` detects kernel Landlock support.
- **User/mount namespace helpers** — `unshare_user_and_mount_ns()`, `write_uid_gid_mappings()`, `bind_mount_home()` exist but are **currently disabled** (see below).

### File: `profiles/linux-system.toml`

Built-in profile layer extending `base`. Grants ro on `/usr`, `/bin`, `/lib`, `/lib64`, `/usr/lib`, `/usr/lib64`, `/etc`, `/dev`, `/proc`; rw on `/tmp`, `/var/tmp`, `/var/cache`, `/run/user`.

### Field tests (`src/bin/isol8-field-test.rs`)

Scenarios 1–7 (cross-platform) + scenarios 10–16 (Linux-specific) are enabled and enforced on Linux.

---

## Resolved issues (2026-06-22)

### Ancestor over-granting (FIXED)

**Root cause:** Landlock's `PathBeneath` grants access to the entire subtree beneath a directory. The old ancestor logic added rules for parent directories needed for path resolution (R2.3), but this inadvertently granted access to sibling directories. For example:

- Metadata grants for `~/.config`, `~/.cache`, `~/.local` → ancestor `/home` added → entire `/home/` accessible
- CWD auto-grant (`/mnt/c/works/.../isol8-wsl`) → ancestors `/mnt/c/...` added → entire `/mnt/c` accessible

**Fix:** Removed the ancestor metadata loop entirely. Landlock's `PathBeneath` is incompatible with stat-only access. Unix DAC already allows path traversal — the child process can traverse parent directories to reach granted paths. Landlock only restricts which directory FDs can be opened.

### `/` ancestor rule (FIXED)

The `/` literal grant in `linux/system-runtime.toml` was misleading. `MatchKind::Literal` is skipped by `build_landlock_rules()` (only `Subpath` is processed), so it had no effect. Removed it.

### Landlock enforcement verified on WSL2

Landlock IS enforced on WSL2 kernel 5.15 (`CONFIG_SECURITY_LANDLOCK=y`):
- `/` and `/srv` are correctly denied (not in any grant)
- `/etc/shadow` is correctly denied (Unix perms + no Landlock grant)
- After the ancestor fix, `/home/` is no longer over-granted

---

## Namespace helpers (disabled)

The following functions exist but are NOT called from `child_setup_and_exec()`:

- `unshare_user_and_mount_ns()` — `unshare(CLONE_NEWUSER | CLONE_NEWNS)`
- `write_uid_gid_mappings()` — writes `/proc/self/uid_map` and `/proc/self/gid_map`
- `bind_mount_home()` — bind-mounts replacement HOME over real home

**Why disabled:** WSL2's kernel allows user namespaces (`unshare --user --mount id` succeeds), but some VM environments (OrbStack) block writes to `/proc/self/uid_map`. The helpers can be re-enabled when user namespaces are confirmed available.

---

## Remaining work

1. **Re-enable namespace helpers** — On systems that support `uid_map` writes, enable user + mount namespaces for robust HOME replacement (R4.6).
2. **Network tiers (R5)** — N0/N1/N2/N3 still unimplemented.
3. **Resource limits (R1.3)** — CPU/mem/PID limits not yet wired.
4. **`--env-file`** — Not yet implemented.
