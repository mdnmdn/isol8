# Linux Backend — Current State & Known Issues

**Date:** 2026-06-20  
**Status:** Landlock compiles and reports `FullyEnforced`, but does not actually restrict filesystem access.

---

## What's implemented

### File: `src/backends/linux.rs`

- **Landlock ruleset builder** — converts `Profile.paths` into `BitFlags<AccessFs>` rules (deny-by-default).
- **`render_policy()`** — prints human-readable Landlock rules for `--dry-run`.
- **`spawn()`** — forks a child, applies `PR_SET_NO_NEW_PRIVS` + Landlock, then `execvp` the target.
- **Ancestor metadata** — adds read-only rules for parent directories needed for path resolution (R2.3), skipping `/` to avoid granting the entire filesystem.
- **Match-kind filter** — skips `literal`/`prefix`/`regex` grants (macOS Seatbelt matchers with no Landlock equivalent); only `subpath` grants are enforced.
- **User/mount namespace helpers** — `unshare_user_and_mount_ns()`, `write_uid_gid_mappings()`, `bind_mount_home()` exist but are **currently disabled** (see below).

### File: `profiles/linux-system.toml`

Built-in profile layer extending `base`. Grants ro on `/usr`, `/bin`, `/lib`, `/lib64`, `/usr/lib`, `/usr/lib64`, `/etc`, `/dev`, `/proc`; rw on `/tmp`, `/var/tmp`, `/var/cache`, `/run/user`; literal `/`.

### Dependencies added to `Cargo.toml`

```toml
[target.'cfg(target_os = "linux")'.dependencies]
landlock = "0.4"
nix = { version = "0.31", features = ["sched", "mount", "fs", "user"] }
enumflags2 = "0.7"
```

---

## OrbStack VM setup

### Creating the VM

```sh
orbctl create --memory 8G --cpus 4 --disk 64G ubuntu:24.04 isol8-linux
```

### Installing Rust

```sh
orb sh -c 'curl -sSf https://sh.rustup.rs | sh -s -- -y'
```

### Project access

The macOS filesystem is shared at `/mnt/mac/Users/mdn/works/projects/agent-manager/workspace/isol8/` inside the VM.

### Running commands in the VM

```sh
orb sh -c '. $HOME/.cargo/env && cd /mnt/mac/Users/mdn/works/projects/agent-manager/workspace/isol8 && cargo build'
orb sh -c '. $HOME/.cargo/env && cd /mnt/mac/Users/mdn/works/projects/agent-manager/workspace/isol8 && cargo test'
```

### Kernel info

```
Kernel: 7.0.11-orbstack-00360-gc9bc4d96ac70
LSM:    capability,landlock,yama,bpf
Config: CONFIG_SECURITY_LANDLOCK=y
```

---

## Tests that pass

```
backends::linux::tests::access_for_ro_rw_none
backends::linux::tests::build_rules_empty_profile
backends::linux::tests::build_rules_none_omitted
backends::linux::tests::build_rules_ro_rw
backends::linux::tests::exit_code_normal
env::tests::allowlist_filters_secrets
env::tests::profile_env_is_default_no_override
env::tests::home_applied_first_and_authoritative
home::tests::expand_tilde_*
home::tests::seed_copies_readonly
profile::tests::merge_*
profile::tests::resolve_requires_*
profile::tests::deny_unknown_fields_rejects_typo
tests/profile_merge.rs (8 integration tests)
```

**Total: 31 tests, all green.**

---

## Sandbox smoke tests (manual)

| Command | Expected | Actual | Status |
|---------|----------|--------|--------|
| `isol8 run --profile linux-system -- echo hi` | `hi` | `hi` | ✅ |
| `isol8 run --profile linux-system -- ls /root/` | Permission denied | Permission denied | ✅ |
| `isol8 run --profile linux-system -- cat /etc/passwd` | readable (in profile) | readable | ✅ |
| `isol8 run --profile linux-system -- ls /home/` | **Permission denied** | `mdn` (accessible) | ❌ |
| `isol8 run --profile linux-system -- cat /home/mdn/test_secret.txt` | **Permission denied** | content leaked | ❌ |

**Dry-run output** correctly shows `/home` is NOT in the grant list. Landlock reports `FullyEnforced`. But `/home` remains readable.

---

## The bug: Landlock reports `FullyEnforced` but doesn't restrict access

### Debug output from `apply_landlock()`

```
[landlock-debug] rule: /usr BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] rule: /bin BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] rule: /tmp BitFlags<AccessFs>(0b111111001110, ...)
[landlock-debug] rule: /lib BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] rule: / BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] rule: /usr/lib BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] rule: /usr BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] rule: /etc BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] rule: /dev BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] rule: /proc BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] rule: /var/tmp BitFlags<AccessFs>(0b111111001110, ...)
[landlock-debug] rule: /var/cache BitFlags<AccessFs>(0b111111001110, ...)
[landlock-debug] rule: /run/user BitFlags<AccessFs>(0b111111001110, ...)
[landlock-debug] rule: /var BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] rule: /run BitFlags<AccessFs>(0b1100, ReadFile | ReadDir)
[landlock-debug] restrict_self status: FullyEnforced no_new_privs: true
```

**Key observation:** `/` appears in the rules as an ancestor rule (line 5). Even though the code skips `/` in the ancestor loop, it's still present. The ancestors `/var` and `/run` are correctly added for their sub-paths, but `/` should NOT be granted as `PathBeneath` — doing so grants the entire filesystem tree.

### Root cause hypothesis

Landlock's `PathBeneath` with `/` as the directory FD grants access to **everything beneath `/`**, which is the entire filesystem. The code attempts to skip `/` in the ancestor loop, but `/` is still ending up in the rules list — possibly because it's added before the skip check runs, or because the skip condition doesn't match the canonical form.

However, even after removing `/` from the rules entirely, the behavior persists — suggesting the issue may be deeper: **Landlock may not be enforced at all in this OrbStack VM**, despite reporting `FullyEnforced`. OrbStack runs inside a container/hypervisor that may not honor Landlock restrictions on the underlying btrfs rootfs.

### Evidence that Landlock may not be enforced

1. `/proc/self/status` shows `NoNewPrivs: 1` but no Landlock field (Landlock doesn't expose a procfs entry, so this is expected but unhelpful).
2. The mount namespace shows the real btrfs rootfs — no mount isolation.
3. `/root/` IS denied (returns `Permission denied`), but `/home/` is NOT — inconsistent if Landlock is truly active.
4. The `sandboxer.rs` example from the landlock crate uses `set_compatibility(CompatLevel::HardRequirement)` and calls `restrict_self().expect(...)`. Our code uses `BestEffort` and checks the status manually.

---

## Namespace helpers (disabled)

The following functions exist but are NOT called from `child_setup_and_exec()`:

- `unshare_user_and_mount_ns()` — `unshare(CLONE_NEWUSER | CLONE_NEWNS)`
- `write_uid_gid_mappings()` — writes `/proc/self/uid_map` and `/proc/self/gid_map`
- `bind_mount_home()` — bind-mounts replacement HOME over real home

**Why disabled:** OrbStack's VM blocks writes to `/proc/self/uid_map` after `unshare(CLONE_NEWUSER)`:

```
writing /proc/self/uid_map (did you unshare CLONE_NEWUSER?): Operation not permitted (os error 1)
```

This prevents user namespace setup, which in turn prevents mount namespace operations. Without user namespaces, HOME bind-mounting (R4.6) cannot be done.

### Workaround path

For environments where user namespaces work (bare metal, most VMs), the namespace helpers can be re-enabled by uncommenting the calls in `child_setup_and_exec()`.

---

## Next steps

1. **Verify Landlock enforcement** — Test on bare metal or a KVM VM (not OrbStack) to confirm Landlock actually restricts access. The OrbStack hypervisor may not pass through Landlock restrictions.

2. **Fix the `/` ancestor rule** — Even if Landlock is enforced, granting `PathBeneath` on `/` defeats deny-by-default. The ancestor logic needs to stat-mount `/` without granting content access, or use a different approach (e.g., grant each ancestor individually up to but not including `/`).

3. **Re-enable namespace helpers** — On systems that support `uid_map` writes, enable user + mount namespaces for robust HOME replacement (R4.6).

4. **Add Landlock ABI version check** — Log the kernel's Landlock ABI version at startup so users can diagnose enforcement issues.

5. **Consider pure-Landlock vs hybrid** — Document when to use pure Landlock (simpler, no ns) vs the hybrid namespace approach (stronger HOME isolation).
