# isol8 — Recipes & Strategies

Recipes package everything needed to make one toolchain work under a **replaced
`$HOME`**: detection metadata, per-strategy home materialization, path grants, and
env vars. They are a **separate document type** from profile layers and compile
down into the existing `Spec` → `effective_policy` pipeline.

Design source: [`inbox/evo-repo.md`](./inbox/evo-repo.md) §4.  
Implementation status: Phases 3–4, 7–8 of
[`wip/multi-evo-plan.md`](./wip/multi-evo-plan.md) (strategies, detect/verify,
offline registries, cage wizard).

---

## 1. Concepts

| Term | Meaning |
|------|---------|
| **Recipe** | Shared TOML document (`kind = "recipe"`) with `id`, `filter`, strategies |
| **Strategy** | `share` \| `link` \| `isolate` — how the tool reconciles with a replaced home |
| **Contribution** | Compiled result: `home_ops` + path grants + env (tokens expanded at resolve) |

Profiles remain raw policy layers. Recipes do **not** go through deny-first merge as
named layers; their grants are appended after the merged profile.

---

## 2. File format

```toml
schema = 1
id = "toolchains/nvm"
kind = "recipe"
filter = { os = ["macos", "linux"] }   # authoritative selector
summary = "Node Version Manager"
tags = ["runtime", "version-manager"]  # optional; wizard grouping / registry search
requires = ["base"]                    # optional; profile layers, not recipes
default_strategy = "link"              # optional

[detect]                               # run by `@cage detect` (host)
probe = { path = "~/.nvm" }
version = "nvm --version"

[verify]
cmd = "node --version"
expect = "^v\\d+"

[strategies.link]
summary = "Run the host's installed versions; new installs land in the cage"
home = [{ kind = "link", from = "#HOME/.nvm", to = "~/.nvm" }]
paths = [
  { path = "#HOME/.nvm", access = "ro" },
  { path = "#HOME/.nvm/alias", access = "rw" },
]
env = { NVM_DIR = "~/.nvm" }
path_prepend = ["~/.nvm/versions/node/*/bin"]

[strategies.isolate]
home = [{ kind = "mkdir", path = "~/.nvm" }]
paths = [{ path = "~/.nvm", access = "rw" }]
env = { NVM_DIR = "~/.nvm" }
```

### Strategies

| Name | Typical use | Materialization |
|------|-------------|-----------------|
| `share` | Warm caches (`~/.m2`, cargo registry) | symlink replaced → real, **rw** on real path |
| `link` | Version managers | symlink + **ro** on real path, rw overlays where needed |
| `isolate` | Agent-writable config | mkdir (and optional seed) under replaced home |

**Wizard defaults** (`@cage new` / `edit` bare tool ids): use the recipe’s
`default_strategy` when set and defined; otherwise heuristics (cache-like ids →
`share`, version-manager-like → `link`, else prefer link > share > isolate among
defined strategies). Explicit `id:strategy` in `--tools` always wins.

### Recipe fields

| Field | Where | Effect |
|-------|-------|--------|
| `tags` | recipe | Labels for wizard grouping / registry search. No policy effect. |
| `requires` | recipe | **Profile layers** the recipe needs. Joined to the layer selection before `resolve_requires`, so they appear in the stack tagged `required`. A layer that does not exist is a hard error. |
| `summary` | strategy | One-line description of the choice (wizard). |
| `danger` | strategy | Why this strategy exceeds the usual ceiling. Printed as a security note before a cage is written; never suppresses the grant. |
| `path_prepend` | strategy | Directories prepended to `PATH` inside the sandbox. |

**`path_prepend`.** `PATH` is a single scalar and env merge is first-writer-wins,
so a recipe cannot contribute to it through `env`. Without this, version managers
that resolve through shims (`~/.pyenv/shims`, `~/.nvm/versions/node/*/bin`, mise)
load but do not work. Semantics:

- Tokens expand as everywhere else (`~` effective home, `#HOME` real home).
- A `*` matches **one whole path segment**, globbed against the filesystem and
  sorted lexically for a deterministic `PATH`. No `**`, no `node-*`.
  Lexical means `v20.20.2` precedes `v24.11.0` and a version manager's own
  "default" alias is **not** consulted: with several versions installed, the
  first match wins. Pin the version in the cage (or narrow the glob) when that
  matters.
- A glob under a path the home plan will *link* is resolved through the link
  target and mapped back, so the first run of a fresh cage produces the same
  `PATH` as the tenth. A glob matching nothing contributes nothing.
- A literal entry is kept even if absent — materialization may still create it.
- Entries are prepended in recipe order, ahead of the inherited `PATH`, after
  `--set-env`; duplicates are dropped (first occurrence wins).
- This widens **lookup, not confinement**: a directory on `PATH` stays unreachable
  unless some layer or recipe granted it. Grant it explicitly.

### Tokens

| Token | Expands to |
|-------|------------|
| `~` / `~/…` | Effective (possibly replaced) home |
| `#HOME` / `#HOME/…` | Real user home |
| `@managed/<id>` | Managed home under platform data dir |

**Symlink grant rule (macOS/Linux):** grants for `link`/`share` must target the
**real** path (`#HOME/…`). A grant only on the symlink under `~` is not enough
for Seatbelt/Landlock to allow reads through the link (see multi-evo Phase 2 field
notes).

Env values are expanded the same way (`NVM_DIR = "~/.nvm"` → absolute under
effective home).

---

## 3. Where recipes are loaded

| Source | Location | Precedence |
|--------|----------|------------|
| Builtin | `recipes/**/*.toml` embedded via `build.rs` | lowest |
| User | `~/.config/isol8/recipes/**/*.toml` | later |
| Registry (offline) | Configured `[registries.*]` path roots or git cache pins | later |
| Explicit | `Spec.recipe_paths` / library `Sandbox::recipe_path` | highest |

Variants of the same `id` must have **disjoint** `filter` selectors *within one
source*; that is an authoring error with no ordering to resolve it. **Across
sources the later one replaces what it overlaps** — a registry may ship a better
`toolchains/cargo` than the embedded recipe without the two colliding. Filename
suffixes (`.windows.toml`) are convention only.

**Registries (Phase 7).** Named path or git sources in `isol8.toml` contribute
recipe directories without network I/O at load time — only after
`isol8 @registry update` (or install) has populated a git cache and `isol8.lock`.
Each recipe’s `source` is labelled `registry:<trust>:<name>:<id>` for detect/verify
trust gating. Unparseable files in a registry tree (e.g. profiles and bundles) are
skipped — but a file that declares `kind = "recipe"` and still fails to parse
prints a warning naming the file and the offending key, because a silently
skipped registry is indistinguishable from an empty one.
See [`registry.md`](./registry.md).

### Per-platform strategy bodies

A strategy name may carry several bodies with disjoint `filter`s, so a recipe whose
platforms differ in one detail need not be split into whole variant files. Write
`[[strategies.<name>]]` (array of tables) instead of `[strategies.<name>]`:

```toml
# macOS and Linux differ only in the build-cache location.
[[strategies.link]]
filter = { os = ["macos"] }
paths = [{ path = "#HOME/Library/Caches/go-build", access = "rw" }]
env = { GOCACHE = "~/Library/Caches/go-build" }

[[strategies.link]]
filter = { os = ["linux"] }
paths = [{ path = "#HOME/.cache/go-build", access = "rw" }]
env = { GOCACHE = "~/.cache/go-build" }
```

Rules, mirroring recipe-level variants:

- Bodies of one strategy must be **disjoint** — two bodies matching one platform is
  a load error, not a precedence question. A body with no `filter` matches
  everything, so it may only appear alone.
- A strategy whose bodies all fail to match is an **error at compile time**
  (`strategy "link" has no body matching this platform`), never a silent empty
  contribution — a strategy that quietly grants nothing looks like a working cage
  until something is denied at runtime.
- The single-table form `[strategies.link]` is unchanged and remains the common
  case; multi-body is opt-in.

Bodies are dispatched on TOML value shape rather than an untagged serde enum, so a
field typo reports `unknown field 'pathz', expected one of ...` with the strategy
named — not `data did not match any variant`.

Bare cage keys normalize to `toolchains/<name>`:

```toml
[toolchains.nvm]          # → toolchains/nvm
strategy = "link"
```

---

## 4. Cage integration

```toml
schema = 1
name = "work"
home = "@managed/work"
profiles = ["base", "macos/system-runtime"]

[toolchains.nvm]
strategy = "link"

[toolchains.cargo]
strategy = "link"
```

```sh
isol8 -c work --show-policies -- node --version
```

Dry-run shows:

```text
-- recipes --
  toolchains/cargo  strategy=link
  toolchains/nvm  strategy=link

-- home --
  materialization plan:
    [apply] link …/.nvm -> …/Users/you/.nvm
```

### Library

```rust
use isol8::{Sandbox, StrategyName};

let code = Sandbox::new()
    .profile("base")
    .home("/tmp/scratch")
    .toolchain("nvm", StrategyName::Link)
    .run(["node", "-v"])?;
```

---

## 5. Built-in recipes (current)

| Id | Default strategy | Platforms |
|----|------------------|-----------|
| `toolchains/nvm` | link | macos, linux |
| `toolchains/cargo` | link | macos, linux |
| `toolchains/maven` | share | macos, linux, windows |

Add more under `recipes/` (embedded at build) or user config.

---

## 6. Detection & verify (runtime)

```toml
[detect]
probe = { path = "~/.nvm" }    # expanded against real home
version = "nvm --version"      # optional; host process when probe hits

[verify]
cmd = "node --version"         # run confined under the cage
expect = "^v\\d+"              # optional stdout pattern (\d / \d+ / ^ / $)
```

| Surface | Action | Where | Side effects |
|---------|--------|-------|--------------|
| `@cage detect` | `stat` probe; optional version cmd | **Host** | None |
| `@cage verify` | materialize home, then `verify.cmd` | **Sandbox** | Home plan apply |
| Normal `isol8 -c …` run | strategies only | Sandbox | Materialize on spawn; no detect/verify |

**Trust:** builtin, local filesystem paths, and registry sources with trust
`official` or `local` may run `detect.version` and `verify.cmd`. Registry recipes
at `community` or `untrusted` block those host commands. Path probes always run
(read-only). See [`registry.md`](./registry.md) §4.

## 7. Not yet

- Bundle documents (`kind = "bundle"`: `recipes` / `[toolchains]` / `[[optional]]`)
  are parsed by the wizard's `--from` only; the recipe loader skips them
- `[[optional]]` recipe sets and cage-level `recipes = [...]` selection
- HTTP registries and artifact signing — deferred after Phase 7 MVP
- Full capability-ceiling enforcement at resolve time (install-time diff only today)
- Wizard extras: full TUI, `@cage clone` / `@cage fix` (Phase 8 core is done —
  managed `[toolchains.*]` + drift; see [`instructions.md`](./instructions.md))
