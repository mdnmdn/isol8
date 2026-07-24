# isol8 — Recipes & Strategies

Recipes package everything needed to make one toolchain work under a **replaced
`$HOME`**: detection metadata, per-strategy home materialization, path grants, and
env vars. They are a **separate document type** from profile layers and compile
down into the existing `Spec` → `effective_policy` pipeline.

Design source: [`inbox/evo-repo.md`](./inbox/evo-repo.md) §4.  
Implementation status: Phases 3–4 of [`wip/multi-evo-plan.md`](./wip/multi-evo-plan.md)
(strategies + detect/verify).

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
default_strategy = "link"              # optional

[detect]                               # Phase 4 will run these
probe = { path = "~/.nvm" }
version = "nvm --version"

[verify]
cmd = "node --version"
expect = "^v\\d+"

[strategies.link]
home = [{ kind = "link", from = "#HOME/.nvm", to = "~/.nvm" }]
paths = [
  { path = "#HOME/.nvm", access = "ro" },
  { path = "#HOME/.nvm/alias", access = "rw" },
]
env = { NVM_DIR = "~/.nvm" }

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
| Explicit | `Spec.recipe_paths` / library `Sandbox::recipe_path` | highest |

Variants of the same `id` must have **disjoint** `filter` selectors (validated on
load). Filename suffixes (`.windows.toml`) are convention only.

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

**Trust:** builtin + local (`~/.config/isol8/recipes/`, `recipe_paths`) may run
`detect.version` and `verify.cmd`. Remote/registry sources (Phase 7) do not until
an explicit trust gate exists. Path probes always run (read-only).

## 7. Not yet

- Remote registry + lockfile — Phase 7
- Wizard-owned managed sections — Phase 8
- `--analyze` denial → recipe suggestion — Phases 5–6
