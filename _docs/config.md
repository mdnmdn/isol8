# isol8 — Configuration

User-facing settings for default profiles, path grants, cages, and offline
registries. Config is loaded once before each run (and by meta-commands such as
`@registry` / `@cage`), then overlaid by environment variables and CLI flags.

**Implementation:** `isol8-core` `config.rs` is the single implementation of
discovery, parse, merge, `@`-expansion, and `ISOL8_*` overrides. `isol8-cli`
`cli/config.rs` is clap glue only (`apply_to_run` maps a loaded `Config` onto the
CLI's `ProfileOpts`); `isol8-registry` re-exports core's discovery for
`[registries.*]` and lockfile placement — neither reimplements it.

Related: [profile-model.md](./profile-model.md), [registry.md](./registry.md),
[instructions.md](./instructions.md).

---

## 1. Discovery order

isol8 resolves **one effective config** as follows:

| Priority | Source | Behavior |
|----------|--------|----------|
| 1 | **`ISOL8_CONFIG_PATH`** | File, or directory containing `isol8.toml` / `isol8.yaml` / `isol8.yml`. Absolute override; **no** project-local merge. |
| 2 | **Project-local marker** in the process cwd | First existing file among: `isol8.toml`, `.isol8.toml`, `encage.toml`, `.encage.toml`. May redirect base config, ignore global, and/or overlay fields. |
| 3 | **OS default directory** | `$XDG_CONFIG_HOME/isol8/` if set; else `~/.config/isol8/` (macOS/Linux); else `%APPDATA%/isol8/` (Windows). Looks for `isol8.toml`, then `isol8.yaml`, then `isol8.yml`. |
| — | **Builtin defaults** | When no file is found (or only a local marker with `ignore_global` and no content): see [§4](#4-builtin-defaults). |

`isol8 @init` writes the OS default file (`~/.config/isol8/isol8.toml` unless
`--path` is given).

**Lockfile** (`isol8.lock`) follows a related rule: prefer `./isol8.lock` when
that file exists or a project marker is present; otherwise
`~/.config/isol8/isol8.lock`. See [registry.md](./registry.md).

---

## 2. Project-local markers

Markers live in the **current working directory** only (no walk-up to git root).

| Filename | Notes |
|----------|--------|
| `isol8.toml` | Canonical project config |
| `.isol8.toml` | Hidden variant (e.g. this repo points at `./_data/config`) |
| `encage.toml` / `.encage.toml` | Alternate names |

Only the **first** existing name in the table above is used.

### Marker-only keys

These appear only in project markers (and are stripped / not part of the
effective `Config` applied to a run):

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `config_path` | string | unset | Redirect the **base** global config. Same semantics as `ISOL8_CONFIG_PATH`: path to a file, or to a directory that contains `isol8.toml` / yaml. Relative paths are relative to the process cwd. |
| `ignore_global` | bool | `false` | If `true`, do not load OS default or `config_path` base; the marker is the whole config (plus builtin fill-ins for empty `default_profiles`). |

### Merge rules (local → base)

1. Resolve **base**:
   - `ignore_global = true` → builtin defaults only  
   - else if `config_path` set → that location (error if missing / invalid)  
   - else → OS default file if present, else builtin defaults  
2. Parse the marker as an **overlay**: only keys **present** in the file replace
   base values. Omitted keys keep the base.  
3. `[registries.<name>]` tables **merge by name** (local wins on collision; base
   names not listed in the marker are kept).  
4. Expand `@` path tokens against the **base config directory** (see [§5](#5-path-token-)).

### Examples

**Redirect to a test/config tree** (same idea as `ISOL8_CONFIG_PATH=./_data/config`):

```toml
# .isol8.toml
config_path = "./_data/config"
```

**Redirect and override one setting:**

```toml
config_path = "./_data/config"
auto_profiles = false
cage = "work"
```

**Standalone project config** (do not pick up `~/.config/isol8`):

```toml
# isol8.toml
ignore_global = true
default_profiles = ["base", "macos/system-runtime"]
auto_profiles = true
add_dirs_rw = ["./"]
```

**Overlay onto the user global config** (no `config_path`):

```toml
# only these fields change; everything else comes from ~/.config/isol8/isol8.toml
cage = "dev"
add_dirs_rw = ["/Users/me/project"]
```

---

## 3. Config parameters

Format: TOML (preferred) or YAML (`.yaml` / `.yml` under OS path or
`ISOL8_CONFIG_PATH`). Unknown top-level keys are **rejected**
(`deny_unknown_fields`), except free-form `[registries.*]` tables which are
parsed separately.

| Parameter | Type | Default (when unset in a full base file) | CLI / env equivalent | Description |
|-----------|------|------------------------------------------|----------------------|-------------|
| `default_profiles` | list of strings | `["base", "<os>/system-runtime"]` | `--profile` / `ISOL8_PROFILE` | Profile layers always selected (deny-first merge order). Empty list in a base file is treated as “use builtin defaults”. |
| `auto_profiles` | bool | `true` | `--auto-profiles` / `--no-auto-profiles` / `ISOL8_AUTO_PROFILES` | When true, also select layers whose `filter.executables` match the command (e.g. `claude` → `agents/claude-code`). |
| `profile_paths` | list of paths | `[]` | `--profile-path` / `ISOL8_PROFILE_PATH` | Extra profile files or directories (later wins on layer name collision with builtins and `~/.config/isol8/profiles/`). Missing paths are a hard error at load. |
| `add_dirs_rw` | list of paths | `[]` | `--add-dirs-rw` / `ISOL8_ADD_DIRS_RW` | Extra read-write path grants. |
| `add_dirs_ro` | list of paths | `[]` | `--add-dirs-ro` / `ISOL8_ADD_DIRS_RO` | Extra read-only path grants. |
| `home` | string or omit | unset | `--home` / `ISOL8_HOME` | Replacement `$HOME` for the confined process. Unset means real home unless a cage/profile opts into replacement. |
| `cage` | string or omit | unset | `-c` / `--cage` / `ISOL8_CAGE` | Named cage to load when CLI/env do not set one. Cage **files** are loaded from `{effective_config_dir}/cages/` (and project `.isol8/cages/`); the effective dir follows `config_path` / `ISOL8_CONFIG_PATH`. See [instructions.md](./instructions.md). |
| `dry_run` | bool | `false` | `--dry-run` / `--show-policies` / `ISOL8_DRY_RUN` | If true, print effective policy and exit (when CLI did not already request a dry-run). |
| `[registries.<name>]` | tables | none | `@registry` CLI | Offline recipe sources. See [§6](#6-registries) and [registry.md](./registry.md). |

### Full example (`~/.config/isol8/isol8.toml`)

```toml
# isol8 configuration
default_profiles = ["base", "macos/system-runtime"]
auto_profiles = true
profile_paths = []
# profile_paths = ["/path/to/extra-profiles", "@/profiles"]
# cage = "work"
add_dirs_rw = []
add_dirs_ro = []
# home = "/tmp/scratch-home"
# dry_run = false

# [registries.policy]
# path = "~/src/isol8-recipes"
# # or: git = "https://…/isol8-recipes.git"
# #     ref = "v1"
# #     trust = "community"
```

YAML shape (same fields):

```yaml
default_profiles:
  - base
  - macos/system-runtime
auto_profiles: true
profile_paths: []
add_dirs_rw: []
add_dirs_ro: []
```

---

## 4. Builtin defaults

Used when no config file exists, or when a base file leaves `default_profiles`
empty:

| Field | Value |
|-------|--------|
| `default_profiles` | `base` + `macos/system-runtime` \| `linux/system-runtime` \| `windows/system-runtime` (by host OS) |
| `auto_profiles` | `true` |
| lists / optional fields | empty / unset |

Written by `isol8 @init` as a starter template.

---

## 5. Path token `@`

Any path that **starts with `@`** is rewritten relative to the **effective
config directory** — the same root after `ISOL8_CONFIG_PATH` / project marker
`config_path` / OS default. With `.isol8.toml` → `config_path = "./_data/config"`,
that root is `./_data/config` (not `~/.config/isol8` and not XDG data).

| Input | Meaning |
|-------|---------|
| `@/profiles` or `@profiles` | `{config_dir}/profiles` |
| `@` | `{config_dir}` itself |
| `@managed/<id>` | `{config_dir}/homes/<id>` (durable cage home) |
| `/abs/path` | unchanged (already absolute) |
| `./rel` (non-`@`) | absolutized against **process cwd at resolve time** |

**All resolved paths are absolute** (lexically normalized) so a later `chdir`
cannot retarget config roots, `@managed` homes, profile paths, or registry
paths. `config_path` itself is also absolutized when the effective config root
is computed.

---

## 6. Registries

Named under `[registries.<name>]`. Each entry must set **exactly one** of
`path`, `git`, or `url` / `http`:

| Field | Required | Meaning |
|-------|----------|---------|
| `path` | one of three | Local directory (checkout or plain folder). May use `@` or `~`. |
| `git` | one of three | Clone URL; content used offline from cache after `@registry update`. |
| `url` / `http` | one of three | HTTP tree — accepted in schema, **not implemented** yet. |
| `ref` | no | Git branch/tag/ref (default `main`). |
| `trust` | no | `official` \| `community` \| `local` \| `untrusted`. Defaults: path→local, git→community, url→untrusted. |

```toml
[registries.official]
git = "https://github.com/example/isol8-recipes.git"
ref = "v1"
trust = "official"

[registries.scratch]
path = "@/registries/scratch"
```

Full registry layout, lockfile, and CLI: [registry.md](./registry.md).

Registries are loaded with the **same discovery and merge** as the rest of
config (env → marker overlay → OS base), including `@` expansion on path
sources.

---

## 7. Precedence (config → env → CLI)

Each step below fills **only the fields still unset** by an earlier one — nothing
is ever clobbered once set:

```
CLI flags (as typed — wins for anything the user set)
  → cage overlay (-c / ISOL8_CAGE / config `cage`; fills fields still unset)
  → config, already resolved:
        builtin defaults
          → base config file (OS / config_path / ISOL8_CONFIG_PATH)
          → project marker overlay (if any)
          → ISOL8_* environment overrides
```

**Cage before config defaults.** The cage is the more specific selection, so its
`profiles` / `home` / `add_dirs_*` / `toolchains` win over `default_profiles` and
friends — but never over something the CLI (or an embedder's pre-set `Spec`
fields) already set. `isol8_core::resolve::spec_from_config` and the CLI's
`apply_cage_to_opts` + `apply_to_run` both implement this same fill-only-empty
chain; see [embedding.md](./embedding.md) §3 "Build a Spec directly".

**`ISOL8_*` applies to the config, not to CLI flags.** Env overrides are folded
into the loaded `Config` (`apply_env_overrides`) *before* anything looks at CLI
flags or the cage, so a flag you actually typed always wins over an `ISOL8_*`
var. This was previously a bug: `ISOL8_PROFILE=base isol8 --profile
toolchains/rust` silently dropped `--profile`. Env now only ever competes with
the config file, never with something you typed on the command line.

| Env var | Effect |
|---------|--------|
| `ISOL8_CONFIG_PATH` | Config file or directory; skips project merge |
| `ISOL8_PROFILE` | Comma- or colon-separated profile layers (replaces `default_profiles` for the run) |
| `ISOL8_PROFILE_PATH` | Extra profile path entries |
| `ISOL8_ADD_DIRS_RW` | Extra rw directories |
| `ISOL8_ADD_DIRS_RO` | Extra ro directories |
| `ISOL8_HOME` | Replacement home |
| `ISOL8_AUTO_PROFILES` | `1` / `true` / `yes` / `on` → true; ignored if CLI set `--auto-profiles` / `--no-auto-profiles` |
| `ISOL8_DRY_RUN` | `1` / `true` / `yes` → dry-run |
| `ISOL8_CAGE` | Named cage (consulted by cage-name resolution, not by the config struct merge) |

List-valued env vars split on `,` or `:`.

---

## 8. Related on-disk layout

Under the OS config directory (`~/.config/isol8/` by default):

| Path | Role |
|------|------|
| `isol8.toml` (or yaml) | Global config file |
| `profiles/**/*.toml` | User profile layers (silent if missing) |
| `recipes/**` | User recipe overlays |
| `cages/*.toml` | Config-level named cages (`@cage list` / `-c`) |
| `homes/<id>/` | `@managed/<id>` durable homes |
| `state.toml` | Wizard managed-toolchain drift state |
| `isol8.lock` | Registry pins when no project lockfile |

When `ISOL8_CONFIG_PATH` or a project marker’s `config_path` redirects the config
root (e.g. to `./_data/config`), **cages, state, registries, `@…` paths, and
`@managed/<id>` homes** are resolved under that tree — not under
`~/.config/isol8/` or `~/.local/share/isol8/`. Project walk-up paths such as
`.isol8/cages/` still apply in addition.

Project-local (cwd / git tree, not all under config):

| Path | Role |
|------|------|
| `isol8.toml` / `.isol8.toml` / `encage.toml` / … | Config markers ([§2](#2-project-local-markers)) |
| `.isol8/cages/`, `.isol8/*.toml` | Project cages |
| `isol8.lock` | Project registry lockfile |

---

## 9. Quick reference

```sh
# Write OS default config
isol8 @init

# Force a config tree (tests, CI)
ISOL8_CONFIG_PATH=./_data/config isol8 --show-policies -- echo hi

# Project redirect (checked into the repo)
# .isol8.toml → config_path = "./_data/config"

# Inspect effective policy (includes config + env + flags)
isol8 --show-policies -- my-agent
```

---

## 10. From a library

`isol8_core::config` is the same discovery/merge/`ISOL8_*` implementation the CLI
uses — no re-registration needed:

```rust
let mut cfg = isol8::config::load()?;          // env → project marker → OS default
isol8::config::apply_env_overrides(&mut cfg);  // ISOL8_PROFILE, ISOL8_HOME, …
```

Reading `HOME` / cwd / `ISOL8_*` from the process environment is the *ambient*
entry point; an in-process host with its own environment should use the
hermetic variant instead:

```rust
let ctx = isol8::Context {
    real_home: "/home/agent".into(),
    cwd: "/srv/work".into(),
    platform: isol8::Platform::Linux,
    config_dir: "/etc/isol8".into(),
    managed_root: "/var/lib/isol8/homes".into(),
};
let cfg = isol8::config::load_in(&ctx)?;
```

Full pipeline, error handling, and the `_in` hermetic variants:
[embedding.md](./embedding.md).
