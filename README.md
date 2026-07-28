# isol8

A lightweight, cross-platform **isolation sandbox for AI coding agents and CLI tools**.
`isol8` wraps an arbitrary command so it runs unprivileged with a deny-by-default, restricted view
of the filesystem, a sanitized environment, an optional replaceable `$HOME`, and (planned)
tiered network confinement.

It generalizes the macOS `sandbox-exec` (Seatbelt) model to **Linux** (Landlock +
namespaces), **WSL2**, and **Windows** (deferred). Primary targets: Linux and macOS.

> **Status (v0.2.6):** **macOS + Linux MVP enforced** (Seatbelt / Landlock; WSL2 5.15
> verified). Path access, HOME replacement, env sanitization, ~70 embedded profiles,
> cages, toolchain recipes, offline registries, cage wizard, and `--analyze` (macOS
> live + NDJSON) are implemented. The repo is a **Cargo workspace**
> (`isol8-core` / `isol8-registry` / `isol8-cli` + facade package `isol8`); binary and
> `use isol8::…` API are unchanged. Network tiers and full Windows path enforcement
> remain deferred.

> Primary inspiration: the macOS [Agent Safehouse](https://github.com/eugene1g/agent-safehouse)
> project, whose composable profile model `isol8` generalizes cross-platform.

Full usage: [`_docs/instructions.md`](_docs/instructions.md).  
Agent/contributor guide: [`AGENTS.md`](AGENTS.md).

## What it does

- **Process isolation** — unprivileged wrapper around any command and its children.
- **Path access control** — per-path `none` / `ro` / `rw` / `metadata`, deny-by-default.
- **Environment isolation** — minimal allowlist; secrets in the host env do not pass through. Opt in per-var with `--env-pass NAME`, or set explicitly with `--set-env K=V`.
- **HOME replacement (opt-in)** — keeps the real `$HOME` by default; substitute a scratch/alternate `$HOME` with `--home`, a cage, or a profile, resolved before any path grant is computed. Seed real-home files read-only (first-creation-only; `--no-seed` to skip), and reach the real home via the `#HOME` token even under replacement.
- **Composable profiles** — ~70 embedded TOML layers, `requires` inheritance, deny-first merge.
- **Conditional filters** — layers and policies can match executable name, OS, and architecture.
- **Auto-profiles** — agent layers (e.g. `claude` → `agents/claude-code`) selected automatically.
- **Cages** — one-knob named selection (`-c work`): home mode + profiles + dirs + toolchains.
- **Recipes & strategies** — toolchain packages (`share` / `link` / `isolate`) with detect/verify.
- **Offline registries** — path/git sources, `isol8.lock`, `@registry update|install` (no network at exec).
- **Cage wizard** — `@cage new` / `@cage edit` (interactive or `--yes`) with managed toolchain sections.
- **Policy diagnosis** — `--analyze` maps denials to recipe suggestions (macOS log scrape or NDJSON feed).

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

# Named cage + wizard
isol8 @cage new work --yes --home managed --tools nvm,cargo --dir "$PWD"
isol8 -c work -- echo hi

# Offline recipe registry (path or git in isol8.toml)
isol8 @registry update
```

**Meta commands** use an `@` prefix so they never collide with the confined program:

```sh
isol8 @profiles-list              # all embedded + user layers
isol8 @profiles-show base         # dump one layer as TOML
isol8 @cage list|show|new|edit|detect|verify
isol8 @registry list|update|install|show|verify
isol8 @diag <cmd>…                # macOS: why did launch abort?
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

Full reference: [`_docs/config.md`](_docs/config.md).

Config discovery order:

1. `ISOL8_CONFIG_PATH` (file or directory) — absolute override
2. Project marker in cwd: `isol8.toml`, `.isol8.toml`, `encage.toml`, `.encage.toml`
   - `config_path = "./_data/config"` redirects the global base (like the env var)
   - `ignore_global = true` uses only the local file
   - other fields merge onto the base (local wins)
3. OS default: `~/.config/isol8/isol8.toml` (`$XDG_CONFIG_HOME/isol8/` when set)

Paths starting with `@` are relative to the config directory
(e.g. `profile_paths = ["@/profiles"]`).

```toml
default_profiles = ["base", "macos/system-runtime"]
auto_profiles = true
profile_paths = []
# cage = "work"

# Optional offline recipe registries:
# [registries.official]
# path = "~/src/isol8-recipes"
# # or: git = "https://…/isol8-recipes.git"  ref = "v1"
```

Environment overrides: `ISOL8_PROFILE`, `ISOL8_PROFILE_PATH`, `ISOL8_ADD_DIRS_RW`,
`ISOL8_HOME`, `ISOL8_CAGE`, `ISOL8_DRY_RUN`, etc.

## Build

Workspace members: `crates/isol8-core`, `crates/isol8-registry`, `crates/isol8-cli`,
and the root facade package `isol8` (default features: `cli` + `registry`).

```sh
cargo build
cargo test --workspace
just ci          # fmt + clippy --workspace + build + test
just field-test  # real sandbox checks (macOS / Linux)
```

### Embedding as a library

Depend on the facade crate; paths like `isol8::Sandbox` stay stable.

```toml
# Cargo.toml — engine only (no CLI / registry / wizard):
isol8 = { path = "../isol8", default-features = false }

# engine + offline registries (optional):
# isol8 = { path = "../isol8", default-features = false, features = ["registry"] }

# + cage authoring API (render / apply / drift / bundles), still no clap:
# isol8 = { path = "../isol8", default-features = false, features = ["wizard"] }
```

```rust
let code = isol8::Sandbox::new()
    .profile("base")
    .grant_rw("/my/project")
    .home("/tmp/scratch")
    .run(["node", "script.js"])?;

// If you load [registries.*] without the CLI binary, call once:
// isol8::ensure_registry_provider();
```

Full API surface — hermetic `_in` variants, `--json` / `Serialize` output,
cages, recipes, detect/verify, the wizard, and Windows caveats:
[`_docs/embedding.md`](_docs/embedding.md). Runnable examples (built by `just ci`):
[`examples/`](examples/).

## Docs

| Doc | Contents |
|-----|----------|
| [`_docs/instructions.md`](_docs/instructions.md) | CLI, cages, wizard, registries, analyze |
| [`_docs/embedding.md`](_docs/embedding.md) | Rust / subprocess embedding guide |
| [`_docs/config.md`](_docs/config.md) | Config discovery, parameters, markers, env |
| [`_docs/profile-model.md`](_docs/profile-model.md) | Profile format, filters, merge |
| [`_docs/recipes.md`](_docs/recipes.md) | Recipes & strategies |
| [`_docs/registry.md`](_docs/registry.md) | Offline registries & trust |
| [`_docs/project-structure.md`](_docs/project-structure.md) | Workspace layout & data flow |
| [`_docs/wip/multi-evo-plan.md`](_docs/wip/multi-evo-plan.md) | Evolution Phases 0–9 done; Phase 10 deferred |
| [`AGENTS.md`](AGENTS.md) | Contributor / agent guide |

## License

[MIT](LICENSE)
