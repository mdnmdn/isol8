# isol8 — Usage Instructions

How to use `isol8` to confine commands, inspect policies, and manage profiles.

> **Platform:** sandbox **enforcement** works on **macOS 12+** (via `sandbox-exec`).
> On Linux and other OSes you can still inspect policies with `--show-policies` and
> `--show-profiles`; running confined commands requires a working backend on that OS.

---

## Command shape

There is **no `run` subcommand**. Pass the command to confine directly after the options:

```sh
isol8 [OPTIONS] <COMMAND> [ARGS]...
```

Run `isol8` with no arguments (or `isol8 --help`) to print usage.

**Meta commands** (config, layer admin) use an `@` prefix so they never collide with
a confined program name:

```sh
isol8 @<meta-command> [OPTIONS] [ARGS]...
```

---

## Quick examples

### Run a command confined

Uses your config defaults (`base` + OS system-runtime) unless you override them:

```sh
isol8 echo hello
isol8 --add-dirs-rw "$PWD" -- make build
isol8 --profile toolchains/rust -- cargo test
```

### Inspect policy without running anything

`--show-policies` prints the layer stack, path grants, environment, and generated
sandbox policy (dry-run style):

```sh
isol8 --show-policies echo hi
isol8 --show-policies --profile agents/claude-code claude --version
```

`--dry-run` is an alias for `--show-policies`.

### See which profile layers apply

`--show-profiles` **without** a command lists every known layer:

```sh
isol8 --show-profiles
isol8 --show-profiles --verbose    # includes requires, filters, policy counts
```

`--show-profiles` **with** a command shows only the layers selected for that run
(including auto-matched agent layers):

```sh
isol8 --show-profiles claude --version
# → base, macos/system-runtime, agents/claude-code, …
```

### First-time setup

Write a default config to `~/.config/isol8/isol8.toml` (or use `--path`):

```sh
isol8 @init
isol8 @init --format yaml --path ~/my-isol8.yaml
```

### Browse built-in layers

```sh
isol8 @profiles-list
isol8 @profiles-list --verbose
isol8 @profiles-show agents/claude-code
```

---

## Options

These flags apply to normal runs and to `--show-policies` / `--show-profiles`:

| Flag | Repeatable | Meaning |
|------|:---------:|---------|
| `--profile <NAME>` | yes | Enable a profile layer (`requires` deps pulled in automatically). |
| `--profile-path <PATH>` | yes | Load layers from a directory or single `.toml` file; overrides same-named builtins. |
| `--auto-profiles` | no | Auto-select layers whose `filter.executables` matches the command name. |
| `--add-dirs-rw <PATH>` | yes | Grant read-write access (top override layer). |
| `--add-dirs-ro <PATH>` | yes | Grant read-only access. |
| `--cwd-ro` | no | Make the auto-granted current working directory read-only (default: it is granted read-write). |
| `--home <PATH>` | no | Replace `$HOME` with `<PATH>` (HOME is otherwise **not** replaced; a profile may also enable a scratch home). |
| `--show-policies` | no | Print effective policy and exit (no execution). |
| `--show-profiles` | no | List all layers, or show layers selected for the given command. |
| `--dry-run` | no | Alias for `--show-policies`. |
| `-v, --verbose` | no | Verbose layer listing (with `--show-profiles` or `@profiles-list`). |

When a flag accepts a path or profile name, you can repeat it.

---

## Meta commands (`@…`)

| Command | Purpose |
|---------|---------|
| `isol8 @init` | Create a default config file. |
| `isol8 @profiles-list` | List all profile layers and their source (builtin, user config, profile-path). |
| `isol8 @profiles-show <NAME>` | Dump one layer as TOML (e.g. `base`, `agents/claude-code`). |
| `isol8 @diag <CMD>...` | Diagnose why a confined command aborts at launch (SIGABRT / exit 134) and report the missing path grant. macOS only. |

Unknown `@` commands print a short hint and exit with an error.

### `@diag` — find the grant a confined command needs to launch

A deny-by-default sandbox aborts a process (SIGABRT, exit 134, no diagnostic) when it
denies a path the runtime needs just to *start* — the root directory `/`, the dyld
shared cache, a dylib dir. `@diag` finds the culprit automatically: it renders the
command's real effective policy, confirms the command launches once read access to
every top-level directory is added, then **dichotomically minimizes** (delta-debug)
that set — re-running the command under each trial policy — until only the grant(s)
whose absence causes the abort remain.

```sh
isol8 @diag node --version
# == isol8 @diag: node --version ==
# 'node --version' is aborted at launch by the current sandbox policy. Searching…
# Found it in 5 trials. 'node --version' launches once the sandbox grants read access to:
#   /
# or add to a profile layer:
#   { path = "/", access = "ro", match = "literal" }
```

Use a fast-exiting probe (e.g. `--version`); a long-running command is killed per
trial and counted as "launched".

---

## Configuration

isol8 reads a global config file before each run. Search order:

1. `ISOL8_CONFIG_PATH` (file, or directory containing `isol8.toml` / `isol8.yaml`)
2. `./isol8.toml` or `./isol8.yaml` in the current directory
3. `~/.config/isol8/isol8.toml` (or `.yaml`)

Example:

```toml
default_profiles = ["base", "macos/system-runtime"]
auto_profiles = true
profile_paths = []
# profile_paths = ["/my/extra-profiles", "/my/override.toml"]
add_dirs_rw = []
```

**Environment overrides** (applied after config, before CLI flags):

| Variable | Effect |
|----------|--------|
| `ISOL8_CONFIG_PATH` | Config file or directory |
| `ISOL8_PROFILE` | Comma-separated `--profile` layers |
| `ISOL8_PROFILE_PATH` | Comma-separated `--profile-path` entries |
| `ISOL8_ADD_DIRS_RW` | Extra read-write directories |
| `ISOL8_ADD_DIRS_RO` | Extra read-only directories |
| `ISOL8_HOME` | Replacement home |
| `ISOL8_DRY_RUN=1` | Same as `--show-policies` |

---

## Built-in profiles

Roughly 70 layers are embedded (Safehouse-derived), including:

| Layer | Role |
|-------|------|
| `base` | Minimal runtime: ro `/usr`+`/bin`, rw `/tmp`, real `$HOME` (replacement is opt-in). |
| `macos/system-runtime` / `linux/system-runtime` | OS essentials (in default stack). |
| `macos-system` / `linux-system` | Backward-compatible aliases. |
| `agents/claude-code` | Auto-selected when the command is `claude`. |
| `toolchains/rust`, `integrations/git`, … | Opt in with `--profile`. |

**Overlay order** (later wins on name collision): builtin → `~/.config/isol8/profiles/` →
`profile_paths` / `--profile-path`.

Custom layers: drop `.toml` files under `~/.config/isol8/profiles/`, or point
`--profile-path` at your own directory.

See [`profile-model.md`](./profile-model.md) for the full schema (`filter`, `[[policies]]`, etc.).

---

## Common workflows

### Confine an AI agent CLI

With `auto_profiles = true` in config (the `@init` default), agent layers match by executable name:

```sh
isol8 --show-profiles claude --version    # preview layers
isol8 --show-policies claude --version    # preview full policy
isol8 --add-dirs-rw "$PWD" claude         # run confined with project write access
```

### Rewrite a command's arguments

A layer can carry a `rewrite` that ensures specific arguments are present on the
confined command (inserted after the program name if missing, left alone if already
there). It is gated by the layer's `filter`, so it only touches matching commands.

Because isol8 already confines the process, a common use is to make a tool skip its
*own* interactive permission prompts. This is **opt-in** — it is not a built-in
default. Author it in your own layer and load it with `--profile-path`:

```toml
# my-rewrites.toml
filter = { executables = ["claude"] }
rewrite = { ensure_args = ["--dangerously-skip-permissions"] }
```

```sh
isol8 --profile-path ./my-rewrites.toml --show-policies claude -p hi
# -- command --
#   claude --dangerously-skip-permissions -p hi
```

A ready-made copy lives at
[`examples/profiles/claude-skip-permissions.toml`](../examples/profiles/claude-skip-permissions.toml).
See [`profile-model.md`](./profile-model.md) for merge rules (args are unioned across layers).

### Override a built-in layer

```sh
# my-override.toml redefines agents/claude-code paths
isol8 --profile-path ./my-override.toml --show-policies claude --version
```

### Developer toolchain

```sh
isol8 --profile toolchains/rust --add-dirs-rw "$HOME/.cargo" -- cargo build
```

### Explicit system profile (legacy name)

```sh
isol8 --profile macos-system --show-policies date
```

---

## What confinement does

- **Filesystem** — deny-by-default. Only merged profile grants are reachable;
  everything else gets `Operation not permitted`. `--add-dirs-rw` / `--add-dirs-ro`
  win over profile layers. The current working directory is auto-granted **read-write** by default; pass `--cwd-ro` to make it read-only.
- **HOME** — resolved before path grants. By default HOME is **not** replaced, so `~`
  in profiles targets your real home (the command's own binary/config stay reachable).
  Pass `--home <dir>` or enable `home_replace` in a layer to substitute a (scratch)
  home; with replacement on, the real home is not granted unless you add it explicitly.
- **Environment** — sanitized to a small allowlist (`HOME`, `PATH`, `SHELL`, `TMPDIR`,
  `USER`, `LOGNAME`, `PWD`). Secrets in the host environment do not pass through.
- **Command** — `isol8` resolves the command against your host `PATH` (like the shell)
  to an absolute path before confining it, and auto-grants read+exec on that binary so
  deny-by-default never hides the command's own executable. A command that isn't on
  `PATH` fails fast with `command "x" not found`.

---

## Troubleshooting

- **`command "x" not found`** — the command isn't on your `PATH`. Use its full path
  (e.g. `isol8 /opt/tool/bin/x …`) or fix `PATH`. isol8 resolves the executable the
  same way the shell does, *before* applying the sandbox.
- **`getcwd: Operation not permitted`** — the working directory is not granted by default.
  Add `--add-dirs-rw "$PWD"` or run from a granted path.
- **Command aborts at launch / exit 134 (SIGABRT), no output** — the sandbox denied a
  path the runtime needs to start. Run `isol8 @diag <command>` to find the missing grant
  (it reports e.g. `{ path = "/", access = "ro", match = "literal" }`), then add it to a
  profile or with `--add-dirs-ro`.
- **`git` / `cargo` fail on macOS** — system shims need extra developer paths. Add
  `--profile toolchains/rust` or grant paths with `--add-dirs-ro`.
- **Policy rejected by sandbox** — use `--show-policies` to print the generated policy
  and see what was emitted before running.
- **No enforcing backend on this OS** — use `--show-policies` to verify the policy;
  execution may fail until the Landlock backend is fully working on your platform.