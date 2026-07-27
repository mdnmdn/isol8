# isol8 — Usage Instructions

How to use `isol8` to confine commands, inspect policies, and manage profiles.

> **Platform:** sandbox **enforcement** works on **macOS 12+** (Seatbelt /
> `sandbox-exec`) and **Linux** (Landlock; WSL2 kernel 5.15 verified). Windows
> AppContainer is a draft (compile + spawn; path grants not yet enforcing — see
> [`windows-support.md`](./windows-support.md)). Use `--show-policies` /
> `--show-profiles` on any OS to inspect the resolved policy without spawning.

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

Common meta commands: `@init`, `@profiles-list`, `@profiles-show`, `@cage`,
`@registry`, `@diag`, `@version`.

---

## Quick examples

### Run a command confined

Uses your config defaults (`base` + OS system-runtime) unless you override them:

```sh
isol8 echo hello
isol8 --add-dirs-rw "$PWD" -- make build
isol8 --profile toolchains/rust -- cargo test
isol8 -c work -- echo hi          # named cage (see Cages below)
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
| `--no-seed` | no | Skip seeding real-home files into the home (overrides profile `home_replace.seed`). |
| `--env-pass <NAME>` | yes | Pass a named variable through from the host env (overrides profile `[env]`). |
| `--set-env <K=V>` | yes | Set an env var explicitly (highest precedence; cannot clear `ISOL8_SANDBOXED`). |
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
| `isol8 @cage …` | Cage admin (list / show / new / edit / detect / verify). See [Cages](#cages-named-selection-units). |
| `isol8 @registry …` | Recipe registries (list / update / install / show / verify). See [Registries](#registries-offline-recipe-sources). |
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

Full reference (all parameters, merge rules, `@` paths, registries):
[`config.md`](./config.md).

isol8 loads config before each run. Discovery order:

1. **`ISOL8_CONFIG_PATH`** (file, or directory containing `isol8.toml` / `isol8.yaml`) —
   absolute override; no project-local merge.
2. **Project-local marker** in the current directory (first match wins):
   `isol8.toml`, `.isol8.toml`, `encage.toml`, `.encage.toml`
3. **OS default:** `~/.config/isol8/isol8.toml` (or `.yaml`), using
   `$XDG_CONFIG_HOME/isol8/` when set (Windows: `%APPDATA%/isol8/`).

When no config file is found, built-in defaults apply (`base` + OS system-runtime,
`auto_profiles = true`).

### Project-local markers

A project marker can:

| Key | Effect |
|-----|--------|
| `config_path = "./_data/config"` | Redirect the global/base config (same semantics as `ISOL8_CONFIG_PATH`: file or directory). Local fields then **merge on top** of that base. |
| `ignore_global = true` | Do not load OS (or redirected) global config; local file is the whole config. |
| other settings | Overlay onto the base: only fields present in the marker replace base values. `[registries.*]` entries merge by name (local wins). |

Example — use a repo test config tree and tweak one flag:

```toml
# .isol8.toml (project root)
config_path = "./_data/config"
auto_profiles = false
```

Example — pure project config, ignore `~/.config/isol8`:

```toml
# isol8.toml
ignore_global = true
default_profiles = ["base", "macos/system-runtime"]
auto_profiles = true
```

### Path token `@`

Paths in config that start with `@` are resolved relative to the **config
directory** (parent of the base config file). Useful for profiles and grants
next to the global config:

```toml
# ~/.config/isol8/isol8.toml
profile_paths = ["@/profiles"]
add_dirs_rw = ["@/../projects/work"]
# [registries.policy]
# path = "@/registries/policy"
```

Non-`@` paths are unchanged (relative paths stay relative to the process cwd).

### Example global config

```toml
default_profiles = ["base", "macos/system-runtime"]
auto_profiles = true
profile_paths = []
# profile_paths = ["/my/extra-profiles", "@/profiles"]
add_dirs_rw = []
```

**Environment overrides** (applied after config, before CLI flags):

| Variable | Effect |
|----------|--------|
| `ISOL8_CONFIG_PATH` | Config file or directory (skips local merge) |
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

## Embedding isol8

`isol8` is a Cargo workspace. Depend on the root **facade** package so
`use isol8::…` stays stable. Engine modules live in `isol8-core` and are re-exported
from the facade. Default features are `cli` + `registry`.

```toml
# Cargo.toml — engine only (no clap / registry / wizard):
isol8 = { path = "../isol8", default-features = false }

# engine + offline registry types (no CLI binary surface):
isol8 = { path = "../isol8", default-features = false, features = ["registry"] }
```

If you use config-backed `[registries.*]` recipe dirs **without** running the
`isol8` CLI binary, call once at startup:

```rust
#[cfg(feature = "registry")]
isol8::ensure_registry_provider();
```

(The shipped `isol8` binary does this for you.)

### `Sandbox` builder

The `isol8::Sandbox` builder mirrors the CLI flags. Choose one of three terminals:

```rust
// run — blocking, returns exit code
let exit: i32 = isol8::Sandbox::new()
    .profile("base")
    .grant_rw("/my/project")
    .home("/tmp/scratch")
    .run(["node", "script.js"])?;

// spawn — non-blocking, returns SandboxChild
let mut child = isol8::Sandbox::new()
    .profile("base")
    .spawn(["sleep", "5"])?;
let code: i32 = child.wait()?;
// child.kill()? to send kill signal

// dry_run — structured policy data, no execution
let dry: isol8::DryRun = isol8::Sandbox::new()
    .profile("base")
    .dry_run(["node", "x"])?;
// dry.policy, dry.env, dry.layer_names, …
```

Available builder methods:

| Method | Equivalent CLI flag |
|--------|---------------------|
| `.profile("name")` | `--profile` |
| `.profile_path(p)` | `--profile-path` |
| `.auto_profiles(bool)` | `--auto-profiles` |
| `.grant_rw(path)` | `--add-dirs-rw` |
| `.grant_ro(path)` | `--add-dirs-ro` |
| `.cwd_ro(bool)` | `--cwd-ro` |
| `.home(path)` | `--home` |
| `.no_seed()` | `--no-seed` |
| `.env_pass(iter)` | `--env-pass` |
| `.set_env("K=V")` | `--set-env` |

### Error handling

Engine functions return `isol8::Result<T>` where `isol8::Error` is a typed enum
(via `thiserror`):

```rust
use isol8::{Error, Result};

match isol8::Sandbox::new().run(["x"]) {
    Err(Error::CommandNotFound(name)) => eprintln!("not found: {name}"),
    Err(Error::NestedSandbox) => eprintln!("already inside isol8"),
    Err(e) => return Err(e.into()),
    Ok(code) => std::process::exit(code),
}
```

Key variants: `CommandNotFound(String)`, `InvalidEnv(String)`, `NestedSandbox`,
`UnsupportedOs(&'static str)`, `PolicyRejected(String)`, `Profile(String)`,
`Io(io::Error)`, `Toml(toml::de::Error)`, `Message(String)`.

---

## Cages (named selection units)

A **cage** is a local, named isolation unit: home mode + profile list + path dirs.
It is a *selection* layer — it fills `Spec` fields; it is not itself a profile layer.
See [`wip/multi-evo-plan.md`](./wip/multi-evo-plan.md) Phase 1 and
[`inbox/evo-repo.md`](./inbox/evo-repo.md) §3.

```sh
# create / edit a cage (wizard — see below)
isol8 @cage new work --home managed --tools nvm,cargo --yes
isol8 @cage list
isol8 @cage show work

# run with an explicit cage (short: -c)
isol8 -c work --show-policies -- echo hi
isol8 --cage work claude
```

**Cage file** (`~/.config/isol8/cages/work.toml` or project-local via `--path`):

```toml
schema = 1
name = "work"
home = "@managed/work"    # inherit | ephemeral | @managed/<id> | /path/to/home
profiles = []             # empty → config default_profiles; non-empty replaces them

# isol8:managed — rewritten by `@cage edit`
[toolchains.nvm]
strategy = "link"

# user-owned dirs (wizard preserves these on edit):
# [[dirs]]
# path = "~/work/acme"
# access = "rw"
```

**Home modes**

| Value | Meaning |
|-------|---------|
| `inherit` | Real `$HOME` (default isol8 behaviour) |
| `ephemeral` | Fresh temp dir per run |
| `@managed/<id>` | Durable isol8-managed dir under the platform data dir (`~/.local/share/isol8/homes/<id>` on Unix) |
| absolute / `~/…` | Explicit replacement path |

**Which cage is selected** (first match):

1. `ISOL8_CAGE`
2. `--cage` / `-c`
3. `cage = "…"` in `isol8.toml`
4. `./.isol8/cage.toml` (walk up to git root)
5. `~/.config/isol8/cages/default.toml`

**Precedence:** existing CLI flags (`--profile`, `--home`, `--add-dirs-*`) override
the cage. Config defaults fill whatever is still empty after the cage.

### Cage wizard (`@cage new` / `@cage edit`)

Authors a cage TOML with managed `[toolchains.*]` sections, optional project
dirs, and drift protection so re-runs do not silently clobber hand edits.

```sh
# Interactive (TTY): prompts for home, toolchains, optional project dir
isol8 @cage new work

# Non-interactive (CI / scripts): --yes required when not a TTY
isol8 @cage new work --yes --home managed --tools nvm,cargo:share --dir ~/proj

# Preview only (no write)
isol8 @cage new work --preview --home managed --tools nvm

# Seed from an offline bundle (registry cache or .toml path)
isol8 @cage new work --from bundles/polyglot-agent --yes
isol8 @cage new work --from ./my-bundle.toml --yes

# Project-local cage file
isol8 @cage new work --path ./.isol8/cages --yes --home managed

# Re-run safely: rewrites managed toolchains; preserves [[dirs]]
isol8 @cage edit work --tools nvm,cargo,maven --yes
# Hand-edited [toolchains.*] → refuse unless --force
isol8 @cage edit work --tools nvm --yes --force

# Optional smoke test after write
isol8 @cage new work --yes --home managed --tools nvm --verify
```

| Flag | Meaning |
|------|---------|
| `--yes` / `-y` | Non-interactive; accept flags / defaults |
| `--home` | `inherit` \| `ephemeral` \| `managed` (→ `@managed/<name>`) \| path |
| `--tools` | Comma list: `nvm,cargo:share` (bare id → recipe `default_strategy` or heuristics) |
| `--dir` | Extra `[[dirs]]` path with `rw` (repeatable); user-owned on edit |
| `--from` | Bundle id (`bundles/…`, `official:bundles/…`) or filesystem `.toml` |
| `--profiles` | Comma-separated profile layers |
| `--path` | Output directory for the cage file (default: `~/.config/isol8/cages/`) |
| `--force` | Overwrite existing / ignore managed-section drift |
| `--preview` | Print generated TOML only (does not write) |
| `--verify` | Run `@cage verify` after a successful write |

**Behaviour notes**

- Always prints the `@cage detect` table first (even with `--yes`).
- Interactive when stdin/stdout are a TTY and `--yes` is not set; otherwise
  non-interactive requires `--yes` or `--preview`.
- Without `--tools` in non-interactive mode, every **found** detect hit is
  selected with default strategies.
- Managed sections are marked `# isol8:managed`. Hashes live in
  `~/.config/isol8/state.toml`. Hand-edited toolchains need `--force` to rewrite.
- Before write, security-relevant **rw** grants on real home (`#HOME`) from the
  chosen strategies are printed.
- `home = inherit` with toolchains is allowed: grants still apply;
  materialization under `~` targets the **real** home (warning).

### Home materialization (plan / apply)

Profile seeds and library `Spec.home_ops` are planned without side effects, then
applied only on real spawn (not on `--show-policies`). Dry-run prints the plan:

```text
-- home --
  path = /tmp/scratch
  materialization plan:
    [apply] mkdir /tmp/scratch
    [apply] seed-ro /Users/you/.gitconfig -> /tmp/scratch/.gitconfig
```

Ops (via `Sandbox::home_op` / `HomeOpSpec`): `link`, `mkdir`, `seed-ro`, `copy`.
Tokens: `~` (effective home), `#HOME` (real home), `@managed/<id>`.

**Symlink tip (macOS/Linux):** a grant on the *link path alone* is not enough to
read through a symlink into the real home — also grant the **target** (`#HOME/…`).

### Toolchain recipes

Cage files can select recipes (see [`recipes.md`](./recipes.md)):

```toml
[toolchains.nvm]
strategy = "link"      # share | link | isolate

[toolchains.cargo]
strategy = "link"
```

```sh
isol8 -c work --show-policies -- node --version
# shows -- recipes -- and materialization plan (links into real ~/.nvm, etc.)
```

Built-ins: `toolchains/nvm`, `toolchains/cargo`, `toolchains/maven` (under
`recipes/`). User overlays: `~/.config/isol8/recipes/`. Offline registry caches
(see [Registries](#registries-offline-recipe-sources)) load by bare id as well.

### `@cage detect` — discover toolchains (read-only)

Lists every **platform-matching** recipe and whether its probe path exists on the
host. Optional `detect.version` runs **on the host** (not confined) when the probe
hits. No home materialization, no sandbox spawn.

```sh
isol8 @cage detect
# Detected in ~:
#   ✓ cargo        /Users/you/.cargo
#   ✓ nvm          /Users/you/.nvm
#   · maven        /Users/you/.m2  not found
```

### `@cage verify` — smoke-test a cage

Materializes the cage home (same plan/apply path as a real run), then runs each
selected recipe’s `verify.cmd` **inside** the sandbox. Optional `verify.expect`
must match stdout.

```sh
isol8 @cage verify work
#   ✓ home             materialized …
#   ✓ nvm          [link] exit 0 → v22.3.0
#   ✗ maven        [share] exit 1 → …
```

Cage resolution matches normal runs (name arg, discovery, or default). Recipes
without `verify.cmd` are skipped. Builtin, local-path, and **official/local
registry** recipes may run version/verify commands; **community** and
**untrusted** registry recipes block those host commands (path probes still run).

### Registries (offline recipe sources)

Named recipe sources configured under `[registries.<name>]` in `isol8.toml`.
Runs never fetch over the network; use `@registry update` (or install when the
cache is empty) to populate the cache and write `isol8.lock`. Full detail:
[`registry.md`](./registry.md).

```toml
[registries.official]
git = "https://github.com/example/isol8-recipes.git"
ref = "main"
trust = "official"          # optional; git default is community

[registries.scratch]
path = "~/src/isol8-recipes"  # default trust: local
```

```sh
isol8 @registry list
isol8 @registry update                 # fetch/refresh + write isol8.lock
isol8 @registry install                # offline open (or fetch), print diff, pin lock
isol8 @registry install --strict       # fail on forbidden/ceiling flags
isol8 @registry show toolchains/sample
isol8 @registry verify                 # lockfile vs on-disk cache
```

Lockfile discovery: `./isol8.lock`, else `~/.config/isol8/isol8.lock`
(`--lockfile PATH` overrides). Git content caches under
`~/.cache/isol8/registries/<name>/<pin>/` (or `XDG_CACHE_HOME`). HTTP registries
are not implemented yet.

### `--analyze` — denial → recipe suggestions

Runs a command and maps **observed** path denials to recipe suggestions: collapsed
roots, matching toolchains, and a flag when the fix is likely a **home link**.

```sh
isol8 --analyze -- node script.js
isol8 -c work --analyze -- claude

# Offline / CI: feed synthetic denials (one JSON object per line)
ISOL8_ANALYZE_FEED=denials.ndjson isol8 --analyze -- echo hi
```

| Platform | Live observation | Offline NDJSON feed |
|----------|------------------|---------------------|
| Any (with feed) | — | Yes |
| **macOS** | **Yes** — unified log (`log stream` + `log show`) | Yes |
| Windows | Not yet (path hook not real; R2 documentary) | Yes |
| Linux | Phase 10 (shadow mode) | Yes |

```sh
# macOS: live deny scrape + recipe suggestions
isol8 --analyze --profile base --profile macos/system-runtime -- /bin/cat ~/.netrc

# macOS only: Seatbelt (trace …) draft allow list (permissive — opt-in)
isol8 --analyze --author --profile base --profile macos/system-runtime -- /bin/ls ~
```

On macOS, `--analyze` is complementary to `@diag`: `@diag` finds **launch** policy
holes (SIGABRT); `--analyze` records **runtime** denials from the unified log.

Reports *observed* denials only — not an audit of every access. Does not edit cages.

**Not yet:** full TUI / `@cage clone` / `@cage fix`, HTTP registries / signing,
auto registry fetch on wizard seed (run `@registry update` first).

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