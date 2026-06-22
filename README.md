# isol8

A lightweight, cross-platform **isolation sandbox for AI coding agents and CLI tools**.
`isol8` wraps an arbitrary command so it runs unprivileged with a deny-by-default, restricted view
of the filesystem, a sanitized environment, an optional replaceable `$HOME`, and (planned)
tiered network confinement.

It generalizes the macOS `sandbox-exec` (Seatbelt) model to **Linux** (Landlock +
namespaces), **WSL2**, and **Windows** (deferred). Primary targets: Linux and macOS.

> **Status: Phase 1 — macOS + Linux MVP working.** Path access, HOME replacement, env
> sanitization, ~70 embedded Safehouse-derived profiles, conditional filters,
> config file + auto-profile selection, and policy introspection are implemented.
> **Enforcement works on macOS** via Seatbelt and **on Linux** via Landlock (WSL2
> kernel 5.15 verified). Network tiers and Windows backend are deferred.

> Primary inspiration: the macOS [Agent Safehouse](https://github.com/eugene1g/agent-safehouse)
> project, whose composable profile model `isol8` generalizes cross-platform.

Full usage: [`_docs/instructions.md`](_docs/instructions.md).

## What it does

- **Process isolation** — unprivileged wrapper around any command and its children.
- **Path access control** — per-path `none` / `ro` / `rw` / `metadata`, deny-by-default.
- **Environment isolation** — minimal allowlist; secrets in the host env do not pass through. Opt in per-var with `--env-pass NAME`, or set explicitly with `--set-env K=V`.
- **HOME replacement (opt-in)** — keeps the real `$HOME` by default; substitute a scratch/alternate `$HOME` with `--home` or a profile, resolved before any path grant is computed. Seed real-home files read-only (first-creation-only; `--no-seed` to skip), and reach the real home from a profile via the `#HOME` token even under replacement.
- **Composable profiles** — ~70 embedded TOML layers, `requires` inheritance, deny-first merge.
- **Conditional filters** — layers and policies can match executable name, OS, and architecture.
- **Auto-profiles** — agent layers (e.g. `claude` → `agents/claude-code`) selected automatically.

## Quick start

```sh
# Run confined (uses config defaults: base + OS system-runtime)
isol8 echo hello

# Grant read-write access to the current project
isol8 --add-dirs-rw "$PWD" make build

# Preview the effective policy (dry-run, no execution)
isol8 --show-policies echo hi

# See which profile layers apply to a command
isol8 --show-profiles claude --version

# First-time setup: write ~/.config/isol8/isol8.toml
isol8 @init
```

**Meta commands** use an `@` prefix so they never collide with the confined program:

```sh
isol8 @profiles-list              # all embedded + user layers
isol8 @profiles-show base         # dump one layer as TOML
```

Run `isol8` or `isol8 --help` for full usage.

## Profiles

Policy is a stack of composable layers merged deny-first. Layers live as one TOML
file each under `profiles/` (~70 embedded at build time via Safehouse port), with
namespaced ids like `agents/claude-code` and `toolchains/rust`.

```toml
# profiles/agents/claude-code.toml
filter = { executables = ["claude"] }
requires = ["integrations/keychain", "integrations/browser-native-messaging"]
paths = [{ path = "~/.claude", access = "rw" }]
```

**Selection order:** config `default_profiles` → explicit `--profile` →
`auto_profiles` (executable filter match) → transitive `requires`.

**Layer sources** (later wins on name collision): builtin →
`~/.config/isol8/profiles/` → `--profile-path`.

See [`_docs/profile-model.md`](_docs/profile-model.md) for schema (`filter`,
`[[policies]]`, merge rules) and [`_docs/instructions.md`](_docs/instructions.md)
for examples and configuration.

## Configuration

Config file search order: `ISOL8_CONFIG_PATH` → `./isol8.toml` →
`~/.config/isol8/isol8.toml`.

```toml
default_profiles = ["base", "macos/system-runtime"]
auto_profiles = true
profile_paths = []
```

Environment overrides: `ISOL8_PROFILE`, `ISOL8_PROFILE_PATH`, `ISOL8_ADD_DIRS_RW`,
`ISOL8_HOME`, `ISOL8_DRY_RUN`, etc.

## Build

```sh
cargo build
cargo test
just ci          # fmt + clippy + build + test
just field-test  # real sandbox checks (macOS)
```



## License

[MIT](LICENSE)
