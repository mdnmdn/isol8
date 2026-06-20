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

**Phase 1 — macOS MVP working.** The full path/HOME/env pipeline is implemented and
enforced on macOS via Seatbelt:

- **profile** — TOML load (embedded `profiles/*.toml` + user config dir), `requires`
  inheritance (transitive, cycle-detected, deps-first), and deny-first `merge` are real.
  Types carry `Access` (none/ro/rw/metadata), `MatchKind`, and the macOS `capabilities`
  + raw-SBPL block (`MacosExtra`). `#[serde(deny_unknown_fields)]` throughout.
- **home / env** — effective `$HOME` resolved first (`--home` > profile > auto-scratch),
  `~` expanded against it before merge; env sanitized to the allowlist, HOME applied first.
- **macOS backend** — renders the merged profile to SBPL (`(deny default)` + per-grant
  allows/denies, ancestor metadata, typed capabilities, raw passthrough) and runs it under
  `/usr/bin/sandbox-exec -p`. Symlinked paths (`/tmp`→`/private/tmp`, `/var`→`/private/var`)
  are emitted in both forms — Seatbelt matches the literal accessed path, not a canonical one.
- **--dry-run** prints the effective grants, env, command, and the generated SBPL.
- **profiles** — built-in `base` + `macos-system` layers; `isol8 run --profile macos-system`
  confines real commands (`sh`, `env`, `date`, `cat`, …).
- **tests** — unit + integration (`cargo test`) and a real-sandbox field-test binary
  (`just field-test`, scenarios 1–7) prove the OS actually enforces the policy.

**Not yet:** the Linux (Landlock) backend still `bail!`s; network tiers (R5), Phase-2 env
flags (`--env-pass`/`--env-file`), resource limits, and the Windows backend are unstarted.
Known gaps: no auto-grant of the cwd yet (confined tools may hit `getcwd` denials unless the
workdir is added via `--add-dirs-rw`); macOS `git`/`cargo` need extra developer-tool paths
beyond `macos-system`.

## Roadmap

1. **Phase 1** — Core path + HOME MVP (Linux Landlock + macOS Seatbelt); profile
   parser/merger; minimal env sanitization; auto scratch home.
2. **Phase 2** — Full R3 env features, resource limits, `--dry-run` policy dump,
   WSL2 testing, docs.
3. **Phase 3** — Network tiers N1→N2 (pasta)→N3 (helper + nftables); DNS/IPv6/MITM.
4. **Phase 4** — Seccomp profiles, structured audit logs, integration test harness,
   hardening, hybrid isolation modes, packaging.
5. **Phase 5** — Windows backend (AppContainer + Job Objects + WFP), best-effort HOME.

## Working directives

How to work in this repo. These are not suggestions.

- **Don't improvise — ask.** If a requirement, an edge case, or a design choice is
  unclear, stop and ask. A wrong guess costs more than a question.
- **KISS — don't overcomplicate.** Simplest solution that is solid wins. No
  speculative abstractions, no config for values that never change, deny-by-default
  stays simple. Fewest moving parts that correctly enforce the policy.
- **Implement *and* check tests.** Every non-trivial change ships with its test
  (unit for logic, a field scenario for enforcement). See
  [`_docs/testing-strategies.md`](_docs/testing-strategies.md). Run them; don't
  assume green.
- **Use subagents to optimize work.** Delegate mechanical or parallelizable work to
  cheaper models (e.g. Haiku/Sonnet) — bulk edits, searches, boilerplate — and
  reserve the strong model for design and security-sensitive code.
- **End every task with the full gate.** Run `just ci` (or equivalently
  `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo build`, `cargo test`).
  A task is not done until compile + test + lint + clippy are clean.
- **Be enterprise-ready.** Solid error handling, no panics on user input, clear
  messages, no silent loss of confinement, reproducible builds. Security correctness
  is never traded for brevity.
- **Update the docs after each task.** Keep `AGENTS.md`, the `_docs/*` specs, and the
  README in sync with what the code actually does.

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
just field-test          # real-sandbox field tests (macOS)

# run a command confined (macOS):
isol8 run --profile macos-system --add-dirs-rw /my/project -- /bin/sh -c 'echo hi'
# inspect the effective policy without running:
isol8 run --profile macos-system --dry-run -- echo hi
```

Full usage: [`_docs/instructions.md`](_docs/instructions.md).
