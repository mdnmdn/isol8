# isol8

A deny-by-default, cross-platform **sandbox for AI coding agents and CLI tools**.
`isol8` wraps an arbitrary command so it runs unprivileged with a restricted view
of the filesystem, a sanitized environment, a replaceable `$HOME`, and (planned)
tiered network confinement.

It generalizes the macOS `sandbox-exec` (Seatbelt) model to **Linux** (Landlock +
namespaces), **WSL2**, and **Windows** (deferred). Primary targets: Linux and macOS.

> **Status: Phase 1 — macOS MVP working.** Path access, HOME replacement, env
> sanitization, profile load/merge/inheritance, and `--dry-run` are implemented and
> **enforced on macOS** via Seatbelt. The Linux (Landlock) backend and the network
> tiers are not wired yet. See [`AGENTS.md`](AGENTS.md) for the detailed status and
> [`_docs/instructions.md`](_docs/instructions.md) for usage.

> Primary inspiration: the macOS [Agent Safehouse](https://github.com/eugene1g/agent-safehouse)
> project, whose composable profile model `isol8` generalizes cross-platform.

## What it does (target)

- **Process isolation** — unprivileged, no-new-privs wrapper; optional resource limits.
- **Path access control** — per-path `none` / `ro` / `rw`, deny-by-default.
- **Environment isolation** — minimal allowlist; explicit opt-in passthrough.
- **HOME replacement** — substitute a scratch `$HOME`, resolved before anything else.
- **Tiered network isolation** — N0 none → N1 proxy → N2 rootless → N3 rooted.
- **Composable profiles** — layered TOML, resolved deny-first, with
  **inheritance** (`requires:`).

## Profiles & inheritance

Policy is a stack of composable profile layers (filesystem grants, env defaults,
network allowlist) merged deny-first. A layer pulls in its dependencies
transitively via `requires:`:

```toml
[profile.git]
requires = ["system-runtime"]
paths = [ { path = "~/.gitconfig", access = "ro" } ]
```

See [`_docs/profile-model.md`](_docs/profile-model.md) for the full schema and
merge semantics, and [`_docs/project-structure.md`](_docs/project-structure.md)
for the code blueprint.

## Build & run

```sh
cargo build
cargo test
```

Run a command confined (macOS):

```sh
# Inspect the effective policy (any OS):
isol8 run --profile macos-system --dry-run -- echo hi

# Confine a command, granting rw to one project directory:
isol8 run --profile macos-system --add-dirs-rw "$PWD" -- /bin/sh -c 'echo built > out.txt'
```

See [`_docs/instructions.md`](_docs/instructions.md) for the full usage guide.

## Docs

- [`_docs/project-description.md`](_docs/project-description.md) — full requirements + Rust ecosystem notes.
- [`_docs/project-structure.md`](_docs/project-structure.md) — target layout & code blueprint.
- [`_docs/profile-model.md`](_docs/profile-model.md) — profile format, inheritance, merge rules.
- [`_docs/instructions.md`](_docs/instructions.md) — usage: commands, flags, profiles, examples.
- [`_docs/testing-strategies.md`](_docs/testing-strategies.md) — unit + cross-platform field tests.
- [`AGENTS.md`](AGENTS.md) — guide for agents working on this repo.

## License

[MIT](LICENSE)
