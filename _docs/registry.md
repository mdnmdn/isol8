# isol8 — Recipe Registries

Registries are **offline-by-default** sources of toolchain recipes (and optionally
profiles). They are configured in `isol8.toml`, fetched only by an explicit
`@registry update` (or an install that needs a missing git cache), and pinned by
`isol8.lock`. Day-to-day sandbox runs never open the network for registry content.

Design source: [`inbox/evo-repo.md`](./inbox/evo-repo.md) §5 / §7.5.  
**Status:** Phase 7 done (v0.2.6) — see
[`wip/multi-evo-plan.md`](./wip/multi-evo-plan.md) (`src/registry.rs`).  
Wizard (`@cage new --from …`) consumes offline registry indexes (Phase 8).

---

## 1. Concepts

| Term | Meaning |
|------|---------|
| **Registry** | A named source of artifacts (recipes, profiles, future bundles) |
| **DirSource** | On-disk tree: `registry.toml` + `index.json` + files |
| **Trust** | How much isol8 trusts that source for install policy and host commands |
| **Lockfile** | `isol8.lock` — registry pins + optional per-entry sha256 |
| **Cache** | Local mirror of git registries under `~/.cache/isol8/registries/` |

A registry is **not** a profile layer and is not selected with `--profile`. Cached
recipes load into `RecipeRegistry` alongside builtins and
`~/.config/isol8/recipes/`, so cages can reference them by bare id (e.g.
`toolchains/sample`).

---

## 2. Configuration

In `isol8.toml` (or the same discovery path as other config), declare named
registries under `[registries.<name>]`. Each table must set **exactly one** of
`path`, `git`, or `url`:

```toml
[registries.official]
git = "https://github.com/example/isol8-recipes.git"
ref = "v1"                 # optional; default "main"
trust = "official"         # optional override

[registries.scratch]
path = "~/src/isol8-recipes"
# trust defaults to "local" for path sources

# HTTP is accepted in config but not implemented yet:
# [registries.cdn]
# url = "https://example.com/isol8-recipes/"
```

| Field | Required | Meaning |
|-------|----------|---------|
| `path` | one of three | Local directory (checkout or plain folder) |
| `git` | one of three | Clone URL; content lives in the cache after update |
| `url` / `http` | one of three | HTTP tree — **not implemented** (errors on open/update) |
| `ref` | no | Git branch / tag / ref (default `main`) |
| `trust` | no | Override: `official` \| `community` \| `local` \| `untrusted` |

**Default trust when `trust` is omitted:**

| Spec | Default trust |
|------|----------------|
| `path` | `local` |
| `git` | `community` |
| `url` | `untrusted` |

The manifest’s `[trust].level` applies when config does not override (and when
opening a path with no override).

---

## 3. On-disk registry layout

```
registry-root/
├── registry.toml          # name, index path, [trust]
├── index.json             # search index (entries with id, kind, file, sha256, …)
└── recipes/
    └── toolchains/
        └── sample.toml    # recipe files (or profiles/ for kind = profile)
```

### `registry.toml` (manifest)

```toml
schema = 1
name = "fixture"
title = "…"
description = "…"
min_isol8 = "0.2.0"        # informational
index = "index.json"

[trust]
level = "official"
forbidden_paths = ["#HOME/.ssh", "#HOME/.aws", "#HOME/.gnupg"]
max_grant_outside_home = "ro"   # ro | rw | none (default treated as rw if omitted)
rw_outside_home_allowed = ["toolchains/sample-cache"]
```

Unknown manifest keys are ignored for forward compatibility.

### `index.json`

Each entry lists `id`, `kind` (`recipe` | `profile` | `bundle`), relative `file`,
optional `sha256`, and recipe metadata (`strategies`, `default_strategy`,
`detects`, `summary`, `os`, …). Fixture example:
[`tests/fixtures/registry/`](../tests/fixtures/registry/).

---

## 4. Trust model

```rust
// TrustLevel — commands_allowed() is true only for Official and Local
official | community | local | untrusted
```

| Level | Typical use | `detect.version` / `verify.cmd` |
|-------|-------------|-------------------------------|
| `official` | Curated / first-party content | Allowed |
| `local` | Path registries, user-authored trees | Allowed |
| `community` | Third-party git registries (default) | **Blocked** |
| `untrusted` | Unknown / HTTP default | **Blocked** |

Path probes (`detect.probe`) always run (read-only). Only **host command**
execution from recipes is gated.

Recipe source labels look like:

```text
registry:<trust>:<name>:<id>
# e.g. registry:official:fixture:toolchains/sample
#      registry:local:scratch:toolchains/nvm
```

`detect::commands_trusted` allows `builtin:…`, local filesystem paths, and
`registry:official:…` / `registry:local:…`. Community and untrusted registry
labels are blocked, as are raw URLs.

---

## 5. Cache and lockfile

### Cache root

- `$XDG_CACHE_HOME/isol8/registries/<name>/<pin>/`
- else `~/.cache/isol8/registries/<name>/<pin>/`

Git update clones/fetches via the **`git` CLI** into a staging dir, then materializes
a pinned tree at `<name>/<commit-sha>/`. Path registries are not copied; the
configured directory is opened in place.

### Lockfile (`isol8.lock`)

Discovery order:

1. `./isol8.lock` if present
2. `./isol8.lock` if a project config (`isol8.toml` / yaml) is in the cwd (created on first update/install)
3. else `~/.config/isol8/isol8.lock` (or under `XDG_CONFIG_HOME`) for user-global registries

Override with `--lockfile PATH` on `@registry` commands.

Contents (TOML):

- **`registries`** — name, source label, pin (commit SHA or content hash), optional
  content hash of `index.json`, trust recorded at lock time
- **`entries`** — per-artifact pins (`registry`, `id`, `kind`, optional `sha256`)

Written by `@registry update` and `@registry install` (unless `--no-lock`).

---

## 6. Offline load into recipes

`RecipeRegistry::load` always stays offline:

1. Builtin recipes (embedded)
2. `~/.config/isol8/recipes/`
3. **Offline registry recipe dirs** — `discover_offline_recipe_dirs()` opens each
   configured registry via `open_offline` (path root or cached git pin). Missing
   caches are **skipped** (no network, no hard error).
4. Explicit `recipe_paths` (highest)

For each registry root, isol8 prefers a `recipes/` subdirectory when present.
Unparseable TOML under a registry tree (profiles, future schemas) is **skipped**
so load does not fail the whole registry.

Git registries that have never been updated are simply absent from the recipe set
until `isol8 @registry update <name>`.

---

## 7. CLI — `isol8 @registry`

```sh
isol8 @registry list
isol8 @registry update [NAME]
isol8 @registry install [NAME]
isol8 @registry show <ID>
isol8 @registry verify
```

| Action | Behaviour |
|--------|-----------|
| **list** | Configured registries, trust, pin, source; offline entry count when openable |
| **update** | Fetch/refresh (git clone/fetch; path re-opens and re-hashes), write lockfile |
| **install** | Open offline if possible, else fetch; print install **diff**; pin lockfile |
| **show** | Look up an index entry by id across offline-openable registries |
| **verify** | Check lockfile pins / content hashes against on-disk cache (drift → error) |

Shared flags:

| Flag | Effect |
|------|--------|
| `--lockfile PATH` | Use this lockfile instead of discovery |
| `--no-lock` | Do not write the lockfile (update/install) |
| `--strict` | On install, fail if any **FORBIDDEN** or **ceiling violation** flag appears |

### Install diff

Compared against previous lock entries for that registry: `added` / `changed` /
`removed` / `same`. Recipe inspection may flag:

- sensitive path markers (`.ssh`, `.aws`, `.gnupg`, …)
- `rw` grants on real home (`#HOME/…`)
- paths listed in manifest `forbidden_paths`
- ceiling violations when `max_grant_outside_home` is `ro`/`none` and the recipe
  is not in `rw_outside_home_allowed`

Without `--strict`, flags are printed but install still completes and pins the
lockfile. **Full capability-ceiling enforcement at resolve/runtime is not
implemented** — install-time diff is the current control point.

---

## 8. Implementer map (`src/registry.rs`)

| Type / fn | Role |
|-----------|------|
| `TrustLevel` | official / community / local / untrusted; `commands_allowed()` |
| `ProfileSource` | `name`, `index`, `trust`, `root`, `get_recipe`, `get_profile` |
| `DirSource` | Open `registry.toml` + index + files |
| `LayeredSource` | Later-wins composition of sources |
| `RegistrySpec` | `Path` \| `Git` \| `Http` (Http errors “not implemented yet”) |
| `Lockfile` / `LockRegistry` / `LockEntry` | Pin storage |
| `open_offline` | No network; git needs lock pin + cache |
| `update_registry` | Path hash / git CLI fetch |
| `diff_index` | Install diff + recipe flags |
| `discover_offline_recipe_dirs` | Ambient config → recipe dirs for `RecipeRegistry` |

Library re-exports (see `src/lib.rs`): `DirSource`, `Lockfile`, `ProfileSource`,
`RegistryIndex`, `RegistrySpec`, `TrustLevel`, and related helpers.

---

## 9. Limits (not in Phase 7)

- **HTTP registries** — config parse only; open/update return a clear error
- **Signing** (minisign / sigstore) — not implemented
- **Async / tokio** — sync only (`git` CLI, filesystem)
- **Capability ceiling at resolve time** — diff/install only for now
- **`registry:` reference syntax in cages** — bare recipe ids work; no separate
  `registry:id` resolver required for offline cache load
- **Optional `registry` cargo feature / crate split** — still in-tree (`isol8`
  engine module); crate split is Phase 9
- **Wizard** — Phase 8 done (`@cage new`/`edit` consumes offline index + detect;
  no auto fetch)

---

## 10. Quick start

```toml
# isol8.toml
[registries.local]
path = "./tests/fixtures/registry"
trust = "local"
```

```sh
isol8 @registry list
isol8 @registry update local
isol8 @registry install local          # review diff, write isol8.lock
isol8 @registry show toolchains/sample
isol8 @registry verify

# Recipes from the cache appear in detect / cages by bare id when offline-openable:
isol8 @cage detect
```
