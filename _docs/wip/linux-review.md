# Branch review — `feature/linux-wsl-support`

Reviewed commit `5aba2f5 draft linux support` (single commit vs `main`).
Scope: Linux Landlock backend changes + field-test extension + docs.

## Summary

The headline change — dropping synthesized Landlock **ancestor rules** — is correct
and well-reasoned. Field tests now exercise real Linux enforcement (scenarios 10–16).
Two issues to fix before this lands: the ABI probe reports the wrong number, and it
confines the `isol8` process as a side effect of a reporting path.

Note: the Linux backend is `cfg(target_os = "linux")`, so it cannot be compiled or run
on the macOS review host. Enforcement claims rest on the field tests having been run on
Linux/WSL2.

## The good (ship-worthy)

- **Removing ancestor rules is the right call.** `PathBeneath` grants the whole subtree,
  so synthesizing ancestor rules (`/home` for `/home/user/.config`) over-grants siblings.
  Unix DAC already handles path traversal; Landlock only restricts which directory FDs can
  be opened, not traversal. Removing the `/` root grant from `system-runtime.toml` is the
  consistent follow-through.
- **Regression is pinned.** `build_rules_no_ancestor_over_granting` asserts no extra
  ancestor rule appears; the updated `build_rules` test asserts exact rule count.
- **Field tests 10–16 exercise enforcement**, not just rendering: deny ungranted path,
  ro/rw, real-home denial, env allowlist. The `outside/` relocation logic is sound — it
  must sit outside `root` because the base grants `/tmp`-adjacent paths, otherwise
  deny-by-default isn't what's under test.

## Issues

### 1. ABI probe reports the wrong number (`src/backends/linux.rs:209`)

```rust
format!("v{} (enforced)", status.ruleset as u8)
```

`status.ruleset` is `RulesetStatus` (`FullyEnforced` / `PartiallyEnforced` /
`NotEnforced`). Casting it to `u8` yields the enum discriminant (0/1/2), **not** the
Landlock ABI version. `--dry-run` therefore prints `Landlock ABI: v0 (enforced)`
regardless of the kernel. The doc comment ("kernel returns the ABI version via
`restrict_self()` status") and `_docs/linux-support.md:83` repeat the same wrong model.

**Fix:** use the crate's real detection — `landlock::ABI::new_current()` — and format that.

### 2. The probe confines the `isol8` process as a side effect (`src/backends/linux.rs:197`)

`probe_landlock_abi()` calls `restrict_self()`, an *irreversible* Landlock restriction on
the current process, inside a function whose only job is to build a `--dry-run` string.
Harmless today (empty `handle_access` restricts nothing) but it is a real confinement
syscall hidden in a reporting path, and it stacks a layer against the kernel's 16-layer
limit. `ABI::new_current()` fixes #1 and #2 together — no ruleset, no `restrict_self`,
no side effect.

### 3. `metadata` silently enforces as `ro` (pre-existing, now reachable)

`Access::Metadata → ReadFile | ReadDir` (`src/backends/linux.rs:131`) grants full subtree
read, but `render_policy` labels it `META`. So the XDG `~/.config` / `~/.cache` /
`~/.local` metadata grants expose full read of those trees while `--dry-run` says `META`.
The Landlock limitation is documented (`_docs/linux-support.md:107`), but the dry-run
output is misleading exactly where confinement reporting matters.

**Fix:** emit `META→ro` (or similar) in the policy dump so the effective grant is honest.

## Minor

- `_docs/wip/lib-structure.md` is untracked — intentional, or a missed `git add`?
- Field-test `real_home` fallback is `/home` on Linux; scenario 14 (`ls /home` denied)
  works because `/home` isn't granted. Fine, just noting the dependency.

## Verdict

Approve direction. Block on #1 and #2 (one small edit: replace the probe body with
`ABI::new_current()`). #3 is a reporting-honesty fix, do it in the same pass.

**Post-review fixes applied (2026-06-22):** All three issues addressed plus
enforcement completeness (comprehensive handled rights so ro actually denies
writes). Field tests 1–16 and units green on WSL2. See `_docs/wip/linux-problems.md`.
