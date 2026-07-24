# isol8 — Agent Guide

## Project

`isol8` is a single-binary Rust CLI: a **lightweight, cross-platform isolation sandbox**
for AI coding agents and CLI tools. It wraps an arbitrary command so it runs
unprivileged with a deny-by-default, restricted view of the filesystem, a sanitized environment, a
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

- `cli` — clap arg parsing, profile selection, invocation overrides. Behind the
  default-on `cli` cargo feature (`cli = ["dep:clap", "dep:serde_yaml", "dep:dialoguer"]`);
  embedders can use `default-features = false` to get the engine without those.
  Layout: `src/cli/{mod,config,diag}.rs`; `src/main.rs` is a thin shim calling
  `isol8::cli::main()`.
- `error` — typed `pub enum Error` (thiserror) + `pub type Result<T>`. Engine modules
  return `isol8::Result`; the CLI layer keeps `anyhow` and upconverts at the boundary.
- `sandbox` — library entry surface: `Spec` (clap-free confinement request), `Sandbox`
  (ergonomic builder with `run`/`spawn`/`dry_run` terminals), `SandboxChild` (non-blocking
  handle with `id`/`wait`/`kill`), `DryRun` (structured policy data, no printing).
- `profile` — `Profile` / `PathGrant` / `Access` / `HomeReplace`, TOML (de)serialization,
  deny-first `merge`. **Drives everything.**
- `env` — minimal sanitized environment construction (HOME first).
- `cage` — named local isolation units: TOML load, discovery, overlay into Spec
  fields (`--cage`/`-c`, `@cage`). Not a profile layer.
- `context` — injectable ambient state (`real_home`, `cwd`, `platform`, `managed_root`).
- `plan` — home materialization plan/apply (`link` / `mkdir` / `seed-ro` / `copy`).
- `recipe` — toolchain recipes (`share`/`link`/`isolate` → grants + home ops + env).
- `registry` — offline-by-default recipe/profile sources (`DirSource`, git cache,
  `isol8.lock`, trust); CLI `@registry list|update|install|show|verify`.
- `detect` — `@cage detect` probes and `@cage verify` in-sandbox smoke tests
  (`commands_trusted` gates official/local registry commands).
- `wizard` — cage authoring (`@cage new` / `edit`): managed `[toolchains.*]`,
  drift via `state.toml`, bundle expand, strategy defaults; CLI uses dialoguer
  when TTY (`toml_edit` + optional `dialoguer` under `cli` feature).
- `analyze` — `--analyze` denials → recipe suggestions (NDJSON feed; shared layer).
- `analyze_macos` — macOS unified-log scrape + optional Seatbelt `(trace …)` for `--author`.
- `backends/{linux,macos,windows}` — render the merged profile into the OS-native
  policy (Landlock ruleset / Seatbelt text / AppContainer) and spawn the command.
  `Backend` trait: `spawn(...) -> Result<SandboxChild>` (non-blocking),
  `render_policy(&self, profile) -> String`.
- `spawn` — cross-platform child execution with policy applied.
- `net` (future) — proxy config + N2/N3 helpers.

**Key invariants:**

- Effective `$HOME` is resolved **before** any path-grant computation.
- **Deny-by-default** everywhere; grants are explicit and unioned deny-first.
- Single unprivileged binary, no persistent daemons.
- Clear **effective-policy reporting** via `--dry-run`.

## Current status

**v0.2.6 — macOS + Linux MVP + evolution Phases 1–8.** Path/HOME/env enforcement works
on macOS (Seatbelt) and Linux (Landlock). Cages, recipes, offline registries, cage
wizard, and `--analyze` are in tree. Network tiers and full Windows path enforcement
remain deferred.

- **profile** — TOML load (`build.rs` embeds all `profiles/**/*.toml` + user config dir +
  `--profile-path` overlays), `requires` inheritance, deny-first `merge`, layer/policy
  `filter` (executable/OS/arch), and auto-profile selection. Types carry `Access`,
  `MatchKind`, `Policy`, `ProfileFilter`, command `rewrite` (`ensure_args`, gated by
  the layer filter and applied to the confined command), and macOS `capabilities` + raw SBPL.
  `#[serde(deny_unknown_fields)]` throughout. ~70 Safehouse-derived layers embedded.
- **home / env** — effective `$HOME` resolved first (`--home` > profile `home_replace` >
  the **real** home; HOME is *not* replaced unless explicitly requested), `~` expanded
  against it before merge; the `#HOME` token expands to the **real** home (survives an
  active replacement); seeding is first-creation-only and `--no-seed` skips it; env
  sanitized to the allowlist, HOME applied first, then `--env-pass`/`--set-env` overrides.
- **executable resolution** — `cmd[0]` is resolved against the host `PATH` (execvp-style)
  to an absolute path before spawning, so a missing command fails with a clean
  `command "x" not found` and the lookup doesn't depend on the in-sandbox PATH; the
  resolved binary is auto-granted `ro` so deny-by-default never hides the command itself.
- **macOS backend** — renders the merged profile to SBPL (`(deny default)` + per-grant
  allows/denies, ancestor metadata, typed capabilities, raw passthrough) and runs it under
  `/usr/bin/sandbox-exec -p`. Symlinked paths (`/tmp`→`/private/tmp`, `/var`→`/private/var`)
  are emitted in both forms — Seatbelt matches the literal accessed path, not a canonical one.
- **Linux backend** — renders the merged profile to Landlock rules (deny-by-default,
  per-path ro/rw) and runs it under `PR_SET_NO_NEW_PRIVS` + Landlock `restrict_self()`.
  No ancestor rules (Landlock's `PathBeneath` grants subtrees, so ancestors would over-grant;
  Unix DAC handles path traversal). ABI version probed at runtime. WSL2 (kernel 5.15)
  verified enforced. Namespace helpers (user/mount) exist but are disabled pending
  `uid_map` write availability.
- **typed errors** — `src/error.rs` defines `pub enum Error` (thiserror) with variants
  `CommandNotFound`, `InvalidEnv`, `NestedSandbox`, `UnsupportedOs`, `PolicyRejected`,
  `Profile`, `Io`, `Toml`, `Message`; all engine modules return `isol8::Result`. The
  CLI layer uses `anyhow` and upconverts at the boundary. A `ResultExt::ctx` helper
  adds context without a full `anyhow` dependency in engine code.
- **library API** — `src/sandbox.rs` exposes `Spec` (plain, clap-free confinement
  request), `Sandbox` (builder: `new()`, `profile`, `profile_path`, `auto_profiles`,
  `grant_rw`, `grant_ro`, `cwd_ro`, `home`, `no_seed`, `env_pass`, `set_env`, then
  `run(cmd) -> Result<i32>` / `spawn(cmd) -> Result<SandboxChild>` /
  `dry_run(cmd) -> Result<DryRun>`), and `ensure_not_nested() -> Result<()>`.
  `src/lib.rs` re-exports engine types including `Error`, `Result`, `Access`,
  `MatchKind`, `PathGrant`, `Profile`, `Cage`, `HomeMode`, `Context`, `HomePlan`,
  `Recipe`, `RecipeRegistry`, `StrategyName`, registry (`DirSource`, `Lockfile`,
  `TrustLevel`, …), analyze (`Denial`, `AnalysisReport`), `confine_executable`,
  `effective_policy`, `Sandbox`, `Spec`, `SandboxChild`, `DryRun`. The `cli`
  module is gated on the `cli` feature.
- **non-blocking spawn** — `SandboxChild` is returned by `Backend::spawn` (and
  `Sandbox::spawn`); methods: `id() -> u32`, `wait() -> Result<i32>`, `kill() ->
  Result<()>`. macOS wraps `std::process::Child`; Linux uses a forked `Pid`
  (`waitpid` on wait); Windows resolves synchronously.
- **structured dry-run** — `DryRun { layer_names, profile, env, cmd, policy,
  policy_label }` is returned by `Sandbox::dry_run` — pure data, no printing. The
  CLI calls `print_dry_run(&DryRun)` to render the text report.
- **cli feature gate** — `cli = ["dep:clap", "dep:serde_yaml", "dep:dialoguer"]`
  is on by default. `isol8 = { ..., default-features = false }` drops clap /
  serde_yaml / dialoguer for engine-only embedding (`toml_edit` stays for
  wizard). The `[[bin]] isol8` has `required-features = ["cli"]`.
- **--dry-run** / `--show-policies` print layer stack + effective grants, env, command, SBPL/Landlock rules.
- **config** — `isol8.toml`/`isol8.yaml` (cwd, `ISOL8_CONFIG_PATH`, or `~/.config/isol8/`),
  `ISOL8_*` env overrides, `isol8 @init`. Defaults: `base` + OS system-runtime; `auto_profiles`
  selects agent layers by executable name (e.g. `claude` → `agents/claude-code`).
  Optional `[registries.*]` for offline recipe sources.
- **Windows backend (Phase 1)** — token-based AppContainer (`DuplicateTokenEx` +
  `SetTokenInformation(TokenAppContainerSid, TokenCapabilities)` +
  `CreateProcessAsUserW`). Supports 12 capability SIDs. Tiers 2–3 deferred.
  `%VAR%` expansion for path grants. System profile with `%SYSTEMROOT%`,
  `%TEMP%` etc. embedded. Compiles on `x86_64-pc-windows-msvc`.
- **CLI** — direct `isol8 CMD` (no `run`); `--show-policies` (layer stack tagged
  explicit/auto/required) / `--show-profiles`; `--analyze` / `--author`; `--no-seed`,
  `--env-pass`, `--set-env`; meta commands `@init`, `@profiles-list`, `@profiles-show`,
  `@cage` (list/show/new/edit/detect/verify), `@registry`
  (list/update/install/show/verify), `@diag`; `--profile-path`.
- **@cage wizard** — `isol8 @cage new|edit <NAME>` (Phase 8): detect table first,
  interactive (dialoguer) or `--yes`/`--preview`; managed `[toolchains.*]` with
  drift protection (`~/.config/isol8/state.toml`); `--from` offline bundles;
  optional `--verify` (`src/wizard.rs`).
- **@registry** — offline-by-default recipe sources; path or git (CLI fetch); lockfile pins.
- **@diag** — `isol8 @diag <cmd>` (macOS) diagnoses launch aborts (SIGABRT/exit 134) by
  delta-debugging the effective Seatbelt policy down to the missing path grant (`src/diag.rs`).
- **profiles** — Safehouse port embedded; `macos-system` / `linux-system` are backward-compat
  aliases. `isol8 echo hi` works without `--profile` when config defaults apply.
- **recipes** — embedded `recipes/toolchains/{nvm,cargo,maven}.toml` plus user/registry overlays.
- **tests** — unit + integration (`cargo test`, including `tests/{registry,wizard,recipe,cage,analyze}.rs`)
  and a real-sandbox field-test binary (`just field-test`, scenarios 1–9 cross-platform,
  10–16 Linux-specific, 17–19 home materialization) prove the OS enforces the policy.

**Not yet:** `--env-file`, resource limits, and network tiers are unstarted. The
Windows (AppContainer) backend is an early draft — it compiles and wires through
the pipeline but does not yet enforce path grants (see `_docs/wip/windows-review.md`).
HTTP registries, registry signing, full TUI / `@cage clone` / `@cage fix`, crate split.
Known gaps: macOS `git`/`cargo` need extra developer-tool paths beyond `macos-system`.

**Evolution track (post-0.2.x):** Phases **0–8 done** — cages, Context/HomePlan, recipes,
detect/verify, shared `--analyze`, macOS log scrape, offline registries
(`src/registry.rs`), cage wizard (`src/wizard.rs`). **Next:** crate split (Phase 9);
Linux shadow observe (Phase 10); Win path hook —
[`_docs/inbox/evo-repo.md`](_docs/inbox/evo-repo.md),
[`_docs/wip/multi-evo-plan.md`](_docs/wip/multi-evo-plan.md),
[`_docs/registry.md`](_docs/registry.md).

## Roadmap

1. **Phase 1** — Core path + HOME MVP (Linux Landlock + macOS Seatbelt + Windows
   AppContainer T1); profile parser/merger; minimal env sanitization; opt-in scratch
   home. **(macOS + Linux MVP done; Windows draft)**
2. **Phase 2** — Full R3 env features, resource limits, `--dry-run` policy dump,
   WSL2 testing, docs. *(partially done: dry-run, env-pass/set-env, WSL2 verified)*
3. **Phase 3** — Network tiers N1→N2 (pasta)→N3 (helper + nftables); DNS/IPv6/MITM.
4. **Phase 4** — Seccomp profiles, structured audit logs, integration test harness,
   hardening, hybrid isolation modes, packaging.
5. **Phase 5** — Windows Job Objects + Low IL + WFP (Tiers 2–3), best-effort HOME,
   `--elevate`/`--no-elevate` flags.
6. **Evolution (parallel track)** — cages → materialization → recipes → detect/verify
   → analyze → registry → **wizard (done)** → crate split (Phase 9). See
   [`_docs/wip/multi-evo-plan.md`](_docs/wip/multi-evo-plan.md).

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
  `toml_edit`, `dialoguer` (cli), `anyhow`. Don't add a dependency for what a few
  lines do.
- **Excellent error messages + `--dry-run`** are first-class: surface *why* something
  was denied and suggest fixes (sysctl, missing package, etc.).

## Build

```sh
cargo build
cargo test
just field-test          # real-sandbox field tests (macOS / Linux)

# run with defaults (base + macos/system-runtime) and auto agent profiles:
isol8 --add-dirs-rw /my/project -- /bin/sh -c 'echo hi'
# inspect layers + policy for a command:
isol8 --show-profiles claude --version
isol8 --show-policies echo hi
# override built-in layers from a file or directory:
isol8 --profile-path ./my-profiles echo hi
# cage wizard + run under a cage:
isol8 @cage new work --yes --home managed --tools nvm,cargo --dir /my/project
isol8 -c work -- echo hi
```

### Embedding isol8 as a library

```toml
# Cargo.toml — engine only (no clap / serde_yaml / dialoguer):
isol8 = { path = "../isol8", default-features = false }
```

```rust
// blocking run:
let exit = isol8::Sandbox::new()
    .profile("base")
    .grant_rw("/my/project")
    .home("/tmp/scratch")
    .run(["node", "script.js"])?;   // -> i32

// non-blocking:
let mut child = isol8::Sandbox::new().profile("base").spawn(["sleep", "5"])?;
let code = child.wait()?;

// structured dry-run (no spawn):
let dry = isol8::Sandbox::new().profile("base").dry_run(["node", "x"])?;
```


## Docs

| Doc | Contents |
|-----|----------|
| [`_docs/instructions.md`](_docs/instructions.md) | User guide: CLI, cages, wizard, registries, analyze |
| [`_docs/profile-model.md`](_docs/profile-model.md) | Profile format, filters, inheritance, merge, status table |
| [`_docs/project-structure.md`](_docs/project-structure.md) | Code layout and data flow |
| [`_docs/project-description.md`](_docs/project-description.md) | Full requirements (R1–R6) |
| [`_docs/testing-strategies.md`](_docs/testing-strategies.md) | Unit + integration + field tests |
| [`_docs/macos-support.md`](_docs/macos-support.md) | macOS Seatbelt: SBPL, capabilities, `@diag`, `--analyze` |
| [`_docs/linux-support.md`](_docs/linux-support.md) | Linux Landlock backend notes |
| [`_docs/windows-support.md`](_docs/windows-support.md) | Windows AppContainer backend (draft) |
| [`_docs/recipes.md`](_docs/recipes.md) | Recipes, strategies, detect/verify |
| [`_docs/registry.md`](_docs/registry.md) | Offline recipe registries: config, CLI, trust, lockfile |
| [`_docs/wip/multi-evo-plan.md`](_docs/wip/multi-evo-plan.md) | Evolution Phases 0–8 done; Phase 9 crate split next |
| [`_docs/inbox/evo-repo.md`](_docs/inbox/evo-repo.md) | Evolution design source (Phases 1–8 implemented) |
| [`AGENTS.md`](AGENTS.md) | Guide for contributors and agents |
