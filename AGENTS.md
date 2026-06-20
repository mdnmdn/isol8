# isol8 — Agent Guide

## Project

`isol8` is a single-binary Rust CLI: a **deny-by-default, cross-platform sandbox**
for AI coding agents and CLI tools. It wraps an arbitrary command so it runs
unprivileged with a restricted view of the filesystem, a sanitized environment, a
replaceable `$HOME`, and (later) tiered network confinement. It generalizes the
macOS `sandbox-exec` (Seatbelt) model to Linux (Landlock + namespaces), WSL2, and
Windows (deferred). **Primary targets: Linux and macOS.**

Primary inspiration: the macOS **Agent Safehouse** project
(<https://github.com/eugene1g/agent-safehouse>), whose composable profile model
isol8 generalizes cross-platform.

Full specification: [`_docs/project-description.md`](_docs/project-description.md).

## Goals / Requirements

- **R1 — Process isolation.** Wrap a command (and its children) as an unprivileged,
  no-new-privs process; optional CPU/mem/PID limits; clean teardown on exit.
- **R2 — Path access (no / ro / rw).** Per-path control, deny-by-default; explicit
  grants only; ancestor metadata-only access for path resolution.
- **R3 — Env isolation.** Start from a minimal allowlist (`HOME`, `PATH`, `SHELL`,
  `TMPDIR`, `USER`, `LOGNAME`, `PWD`); explicit opt-in passthrough.
- **R4 — HOME replacement (first-class).** Substitute an alternate `$HOME`, resolved
  *before* any other path computation; optionally seed it read-only from the real home.
- **R5 — Tiered network isolation.** N0 none · N1 cooperative proxy · N2 rootless
  enforced (pasta) · N3 rooted enforced (netns + nftables); auto-select strongest tier.
- **R6 — Composable profile model.** Layered, numbered profiles resolved deny-first,
  each contributing path grants, env defaults, and network allowlist domains.

## Architecture

Modules (see spec §7):

- `cli` — clap arg parsing, profile selection, invocation overrides.
- `profile` — `Profile` / `PathGrant` / `Access` / `HomeReplace`, TOML (de)serialization,
  deny-first `merge`. **Drives everything.**
- `env` — minimal sanitized environment construction (HOME first).
- `backends/{linux,macos,windows}` — render the merged profile into the OS-native
  policy (Landlock ruleset / Seatbelt text / AppContainer) and exec the command.
- `spawn` — cross-platform child execution with policy applied.
- `net` (future) — proxy config + N2/N3 helpers.

**Key invariants:**

- Effective `$HOME` is resolved **before** any path-grant computation.
- **Deny-by-default** everywhere; grants are explicit and unioned deny-first.
- Single unprivileged binary, no persistent daemons.
- Clear **effective-policy reporting** via `--dry-run`.

## Current status

**Phase 1 skeleton.** Module layout, CLI surface, profile data model, and backend
trait are in place and compile. All behavior is stubbed (`todo!()` / "not yet
implemented"): profile loading + `merge`, env construction, `--dry-run` rendering,
and both the Linux (Landlock) and macOS (Seatbelt) backends. Network tiers (R5) and
the Windows backend are not started.

## Roadmap

1. **Phase 1** — Core path + HOME MVP (Linux Landlock + macOS Seatbelt); profile
   parser/merger; minimal env sanitization; auto scratch home.
2. **Phase 2** — Full R3 env features, resource limits, `--dry-run` policy dump,
   WSL2 testing, docs.
3. **Phase 3** — Network tiers N1→N2 (pasta)→N3 (helper + nftables); DNS/IPv6/MITM.
4. **Phase 4** — Seccomp profiles, structured audit logs, integration test harness,
   hardening, hybrid isolation modes, packaging.
5. **Phase 5** — Windows backend (AppContainer + Job Objects + WFP), best-effort HOME.

## Conventions for agents

- Keep it a **single unprivileged binary**; only the future N3 net helper escalates
  (and drops privilege before exec).
- **Deny-by-default** is the rule — never widen grants implicitly.
- **Resolve `$HOME` first**, before computing any path grants.
- **Profile TOML drives everything** — extend the schema in `profile.rs` (see spec §7).
- Prefer the referenced crates: `landlock`, `nix` (Linux), `clap`, `serde`, `toml`,
  `anyhow`. Don't add a dependency for what a few lines do.
- **Excellent error messages + `--dry-run`** are first-class: surface *why* something
  was denied and suggest fixes (sysctl, missing package, etc.).

## Build

```sh
cargo build
cargo test
# example (once implemented):
isol8 run --profile rust --add-dirs-rw /my/project cargo build
```
