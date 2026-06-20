# isol8

A deny-by-default, cross-platform **sandbox for AI coding agents and CLI tools**.
`isol8` wraps an arbitrary command so it runs unprivileged with a restricted view
of the filesystem, a sanitized environment, a replaceable `$HOME`, and (planned)
tiered network confinement.

It generalizes the macOS `sandbox-exec` (Seatbelt) model to **Linux** (Landlock +
namespaces), **WSL2**, and **Windows** (deferred). Primary targets: Linux and macOS.

> **Status: Phase 1 — macOS MVP working.** Path access, HOME replacement, env
> sanitization, ~70 embedded Safehouse-derived profiles, conditional filters,
> config file + auto-profile selection, and policy introspection are implemented.
> **Enforcement works on macOS** via Seatbelt. The Linux (Landlock) backend and
> network tiers are not fully wired yet. See [`AGENTS.md`](AGENTS.md) for detail.

> Primary inspiration: the macOS [Agent Safehouse](https://github.com/eugene1g/agent-safehouse)
> project, whose composable profile model `isol8` generalizes cross-platform.

## What it does

- **Process isolation** — unprivileged wrapper around any command and its children.
- **Path access control** — per-path `none` / `ro` / `rw` / `metadata`, deny-by-default.
- **Environment isolation** — minimal allowlist; secrets in the host env do not pass through.
- **HOME replacement** — scratch `$HOME`, resolved before any path grant is computed.
- **Composable profiles** — ~70 embedded TOML layers, `requires` inheritance, deny-first merge.
- **Conditional filters** — layers and policies can match executable name, OS, and architecture.
- **Auto-profiles** — agent layers (e.g. `claude` → `agents/claude-code`) selected automatically.
- **Tiered network isolation** *(planned)* — N0 none → N1 proxy → N2 rootless → N3 rooted.

## Quick start

There is **no `run` subcommand** — pass the command to confine directly:

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

## Docs

| Doc | Contents |
|-----|----------|
| [`_docs/instructions.md`](_docs/instructions.md) | User guide: CLI, flags, config, examples |
| [`_docs/profile-model.md`](_docs/profile-model.md) | Profile format, filters, inheritance, merge |
| [`_docs/project-structure.md`](_docs/project-structure.md) | Code layout and data flow |
| [`_docs/project-description.md`](_docs/project-description.md) | Full requirements |
| [`_docs/testing-strategies.md`](_docs/testing-strategies.md) | Unit + field tests |
| [`AGENTS.md`](AGENTS.md) | Guide for contributors and agents |

## License

[MIT](LICENSE)