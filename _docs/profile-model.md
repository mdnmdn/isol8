# isol8 — Profile Model

> The profile is the single source of truth for what a confined process may do.
> A run's effective policy is the **deny-first merge** of an ordered stack of
> profile layers (expanded over their inheritance graph) plus invocation
> overrides. This document defines the on-disk format, inheritance, every field,
> and the merge semantics.
>
> Companion to [`project-description.md`](./project-description.md) (R2–R6) and
> [`project-structure.md`](./project-structure.md) (`profile/` module). Mirrors the
> upstream **Agent Safehouse** composition model (see §9), generalized cross-platform.

---

## 0. Implementation status (Phase 1+)

The loader (`src/profile.rs`, `#[serde(deny_unknown_fields)]`) implements the
portable profile core plus conditional filters. Anything not listed as parsed will
**error on load**, not be ignored:

| Area | Status |
|------|--------|
| File format | **One layer per file** (`profiles/**/*.toml`). Layer id = relative path without extension (e.g. `agents/claude-code`). The `[profile.<name>]` multi-layer form (§4) is **not** parsed yet. |
| Embed | **`build.rs`** walks `profiles/` and emits `profiles_embedded.rs` (~70 Safehouse-derived layers). |
| Layer sources | **Builtin** → **user config dir** (`$XDG_CONFIG_HOME/isol8/profiles/`) → **`profile_paths`** / `--profile-path` (later wins on name collision). |
| Config file | **`isol8.toml` / `isol8.yaml`** (cwd, `ISOL8_CONFIG_PATH`, or `~/.config/isol8/`). `default_profiles`, `auto_profiles`, `profile_paths`, path overrides. |
| Profile language | **TOML** for layers. **TOML or YAML** for the global config file. |
| Fields parsed | `requires`/`extends`, `filter`, `[[policies]]`, `paths`, `env`, `home_replace` (incl. `path`), `rewrite` (`ensure_args`), `macos` (`capabilities`/`raw`). |
| `access` | `none` / `ro` / `rw` / `metadata` — parsed; enforced by the macOS backend. |
| `match` | `subpath` / `literal` / `prefix` / `regex` — parsed; macOS-enforced. Linux: `subpath` only today. |
| Auto-selection | **`auto_profiles`** (config/CLI): layers with non-empty `filter.executables` matching `cmd[0]` basename are added to the stack. |
| `network` block | **Not parsed yet** (Phase 3). Including `network` in a layer fails to load. |
| Enforcement | **macOS** via Seatbelt. **Linux** via Landlock (deny-by-default, per-path ro/rw). |
| Introspection | `isol8 profiles list|show|resolve`, `isol8 policies show`, `--dry-run` (layer stack + effective policy). |

Examples below that include a `network` block illustrate the *target* schema; omit
it to author a layer that loads today.

---

## 1. Concepts

- **Layer** — one named profile fragment (a TOML file). Contributes unconditional
  grants plus optional conditional **policies** (§5). May carry a layer-level
  `filter` that gates its grants by run context.
- **Policy** — a conditional grant bundle: `filter` + `paths` (+ optional `macos`).
  Matching policies are folded into the layer before merge; non-matching policies
  are dropped.
- **Inheritance** — a layer declares prerequisites with `requires` (alias
  `extends`); the set is expanded transitively before merging (§3). `requires`
  edges are unconditional — a dependency is always pulled when its parent is selected,
  but its grants may be empty if the layer filter fails.
- **Stack** — layers selected by config defaults, `--profile`, and auto-profile
  matching (§3), expanded over `requires`, filtered, then merged deny-first into
  one effective `Profile`.
- **Override** — values supplied at invocation (`--add-dirs-rw`, `--home`, …) or
  via `profile_paths` overlay. CLI path overrides are the highest-priority merge layer.

Default for everything is **deny / minimal**. A layer only ever *adds* capability;
the merge decides who wins on conflict (§6).

---

## 2. Layer bands (priority hint)

Layers carry a band, lowest priority first. Bands are a **tiebreaker** for the
inheritance topo-sort (§3) and the default order when no `requires` edge applies —
not the sole ordering authority. Mirrors the Safehouse model (R6):

```
00  base               built-in: minimal system runtime (ro /usr, rw /tmp, env PATH)
10  system-runtime     OS-specific runtime paths, process primitives
20  network            base network tier + DNS
30  toolchains         rust | node | python | … (and auto-detected)
40  shared             shared caches/config common to toolchains
50  integrations-core  always-on integrations (e.g. git)
55  integrations-opt   named extras via --enable: github, npm, keychain, …
60  agents             per-agent layers (claude-code, codex, …)
65  apps               desktop app bundles
--  workdir            cwd granted rw by default (--cwd-ro for ro); ancestors metadata-only
--  custom (CLI)       --add-dirs-rw / --add-dirs-ro / --home / --env-*
--  appended           explicitly appended profiles
```

Layers are selected by:

1. **`default_profiles`** in config (OS-specific: `base` + `macos/system-runtime` or
   `linux/system-runtime`; aliases `macos-system` / `linux-system` still work).
2. **`--profile NAME`** (repeatable) and config `ISOL8_PROFILE`.
3. **`auto_profiles`** — layers whose `filter.executables` contains the command
   basename (e.g. `claude` → `agents/claude-code`).

Transitive `requires` are pulled in automatically. `--enable NAME,…` (Phase 3) will
be an alias for optional integration layers.

---

## 3. Inheritance (`requires`)

A layer lists the layers it depends on. Dependencies are resolved transitively and
folded into the stack **before** the requiring layer, so the requirer wins ties.

```toml
[profile.git]
requires = ["system-runtime", "agent-common"]
paths = [ { path = "~/.gitconfig", access = "ro" } ]

[profile.claude-code]
requires = ["keychain", "browser-native-messaging", "microphone"]
```

Layer ids may be namespaced (`shared/agent-common`, `toolchains/rust`).

**Selection algorithm** (`select_layer_names`, before `resolve_requires`):

1. Start from `default_profiles` (config) and explicit `--profile` names.
2. If `auto_profiles`: scan all known layers; include any whose `filter.executables`
   is non-empty and matches the confined command's basename.
3. Deduplicate, preserving first-seen order.

**Inheritance algorithm** (`resolve_requires`, after selection, before filter/merge):

1. DFS over `requires` from the selected set, collecting every transitive dependency.
3. **Cycle detection** — a back-edge is a hard error reporting the cycle path.
4. **Dedup** — each layer appears once, even if required via multiple paths
   (diamonds: `claude-app → electron → macos-gui` and `… → vscode → macos-gui`
   yield a single `macos-gui`).
5. **Topological order** — dependencies before dependents; ties broken by band
   number (§2), then declaration order. A required layer lands at the earliest
   position that satisfies all its dependents.

The output is an ordered `Vec<Profile>` (deps-first). Each layer then passes through
`filter::apply_layer_filter` (§5.1): layer-level `filter` may zero out grants; matching
`[[policies]]` are folded in. Finally `merge` (§6) runs on the filtered layers plus
the CLI override layer (`--add-dirs-*`).

---

## 4. File format

TOML is primary for layers (built-in `profiles/**/*.toml`, embedded via `build.rs`).
Layers may be namespaced by subdirectory:

```toml
# profiles/toolchains/rust.toml  → layer "toolchains/rust"
requires = ["macos/system-runtime"]
paths = [ { path = "~/.cargo", access = "rw" } ]
```

**Layer overlay sources** (lowest → highest priority on name collision):

| Source | Location | Missing path behaviour |
|--------|----------|------------------------|
| Built-in | `profiles/**/*.toml` (embedded) | N/A |
| User config | `$XDG_CONFIG_HOME/isol8/profiles/**/*.toml` | Silent skip if dir absent |
| Profile path | `--profile-path` / `config.profile_paths` | **Hard error** if path missing |

A profile-path entry may be a **directory** (recurse `**/*.toml`, id = relative path
without extension) or a **single file** (id = filename stem).

**Global config** (`isol8.toml` / `isol8.yaml`) — separate from layer files:

```toml
default_profiles = ["base", "macos/system-runtime"]
auto_profiles = true
profile_paths = ["/proj/my-profiles", "/proj/override.toml"]
add_dirs_rw = []
```

See [`instructions.md`](./instructions.md) for discovery order and `ISOL8_*` env vars.

A file may define one or many layers. Two forms (only the first is parsed — §0):

**One layer per file** — **the supported form**:

```toml
# profiles/agents/claude-code.toml  → layer "agents/claude-code"
filter = { executables = ["claude"] }
requires = ["integrations/keychain", "integrations/browser-native-messaging"]
paths = [ { path = "~/.claude", access = "rw" } ]
```

**Multiple layers in one file** (explicit `[profile.<name>]`) — *target only, not parsed yet (§0)*:

```toml
[profile.base]
paths = [
  { path = "/usr", access = "ro" },
  { path = "/tmp", access = "rw" },
]
env = { PATH = "/usr/bin:/bin" }
home_replace = { enabled = true, auto_scratch = true, seed = ["~/.gitconfig"] }
network = { tier = "n1", allow_domains = ["github.com", "*.githubusercontent.com"] }

[profile.rust]
requires = ["base"]
paths = [ { path = "~/.cargo", access = "rw" } ]
```

YAML equivalent of the `base` layer — *target only, not parsed yet (§0)*:

```yaml
# profiles/base.yaml  → layer "base"
paths:
  - { path: /usr, access: ro }
  - { path: /tmp, access: rw }
env:
  PATH: /usr/bin:/bin
home_replace:
  enabled: true
  auto_scratch: true
  seed: ["~/.gitconfig"]
network:
  tier: n1
  allow_domains: ["github.com", "*.githubusercontent.com"]
```

---

## 5. Schema reference

### Layer (top-level table)

| Field | Type | Default | Req | Meaning |
|---|---|---|---|---|
| `requires` | array of string | `[]` | no | Layers pulled in transitively, deps-first (§3). Alias: `extends`. |
| `filter` | ProfileFilter | unset | no | Layer-level gate (see ProfileFilter + Filter application below). When set and no match, grants/env/home/macos are dropped but `requires` still participates in graph resolution. |
| `policies` | array of Policy | `[]` | no | Conditional grant bundles (see Policy below). |
| `paths` | array of PathGrant | `[]` | no | Unconditional filesystem grants (R2). |
| `env` | map<string,string> | `{}` | no | Env defaults, merged without override (R3.5). |
| `home_replace` | HomeReplace | unset | no | HOME replacement policy (R4). |
| `rewrite` | Rewrite | unset | no | Command rewrite: ensure args are present (see Rewrite below). |
| `network` | NetworkPolicy | unset | no | Network tier + domain allowlist (R5). *Not parsed yet.* |
| `macos` | MacosExtra | unset | no | macOS-only capability grants + raw SBPL passthrough (§8). |

Layer name comes from the file's relative path under its source root (one-per-file
form) or the `[profile.<name>]` table key. It is not a field inside the table.

### ProfileFilter

| Field | Type | Default | Meaning |
|---|---|---|---|
| `executables` | array of string | `[]` | Match `cmd[0]` basename (path stripped, `.exe` stripped on Windows). Empty = no constraint. |
| `os` | array of string | `[]` | Match `macos` / `linux` / `windows`. Empty = no constraint. |
| `arch` | array of string | `[]` | Match `aarch64` / `x86_64` / … Empty = no constraint. |

Multiple fields in one filter use **AND** semantics. A filter with all fields empty
matches every run context.

**Auto-selection** uses only layers with a **non-empty `executables`** list. OS/arch
filters gate grants when a layer is selected (via defaults, `--profile`, requires, or
auto-select) but do not alone trigger auto-selection.

### Policy (`[[policies]]`)

| Field | Type | Default | Meaning |
|---|---|---|---|
| `filter` | ProfileFilter | `{}` | All constraints must match for this policy's grants to apply. |
| `paths` | array of PathGrant | `[]` | Grants contributed when filter matches. |
| `macos` | MacosExtra | unset | macOS extras contributed when filter matches. |

### Filter application

After `resolve_requires`, each layer passes through `apply_layer_filter(ctx)`:

1. If layer `filter` is set and fails → clear `paths`, `env`, `home_replace`, `macos`,
   `policies` (the layer shell remains for ordering; `requires` was already expanded).
2. Fold each `[[policies]]` entry whose `filter` matches into the layer's unconditional
   fields; drop non-matching policies.
3. Feed filtered layers to `merge` (§6).

Dependencies pulled via `requires` are **not** re-filtered by the parent's executable
filter — only each layer's own `filter` / `policies` apply.

### PathGrant

| Field | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | — | Absolute, `~`-prefixed (expands to **effective** home, §7), or containing `#HOME` (expands to the **real** home — survives an active `--home`/`home_replace`, §7). |
| `access` | enum `none` \| `ro` \| `rw` \| `metadata` | — | Deny / read-only / read-write / stat-only (R2.2, R2.3). |
| `match` | enum `subpath` \| `literal` \| `prefix` \| `regex` | `subpath` | How `path` matches: whole subtree / exact node / string prefix / regex. |

- `none` is an **explicit deny** — carve a hole out of a broader grant (e.g. `~` rw
  but `~/.ssh` none). It wins by layer precedence like any other grant (§6).
- `metadata` grants stat-only access for path resolution without content read.
  Ancestors of any granted path get this implicitly.
- `subpath` (the default) covers a directory subtree (Landlock `PathBeneath` /
  Seatbelt `subpath`); `literal`/`prefix`/`regex` mirror Seatbelt matchers and the
  `home-literal` / `home-prefix` macros — these are **macOS-only** (Landlock has no
  prefix/regex matcher; Linux approximates `regex`/`prefix` as nearest subtree with
  a warning).

### HomeReplace

| Field | Type | Default | Meaning |
|---|---|---|---|
| `enabled` | bool | `false` | Turn on HOME replacement. |
| `auto_scratch` | bool | `false` | If no `--home`/path given, create a per-session scratch home (temp dir). |
| `path` | string | unset | Explicit replacement home (overridden by `--home`). |
| `seed` | array of string | `[]` | Real-home entries copied **read-only** into the replacement (R4.4), e.g. `~/.gitconfig`, a scoped `~/.ssh` subset. |

Seeding is **first-creation only**: an entry already present in the (persistent)
home is left untouched, so a re-run never fails trying to overwrite last run's
read-only copy. Pass `--no-seed` to skip seeding entirely (overrides every layer's
`seed`, since `seed` lists otherwise union additively across layers — §6).

HOME replacement is **opt-in**: with no `--home` and no layer enabling
`home_replace`, the effective home is the **real** home (so a command's own
binary/config under `~` stay reachable). Resolution precedence: `--home` > layer
`home_replace.path` > `auto_scratch` temp dir > the real home. When a replacement
*is* active, the real home is **not** granted by default (R4.5); re-add via an
explicit `paths` grant if needed. The home token (`~` / `$HOME`) is isol8's
equivalent of the Safehouse `HOME_DIR` placeholder.

### Rewrite

| Field | Type | Default | Meaning |
|---|---|---|---|
| `ensure_args` | array of string | `[]` | Arguments the confined command must carry. Each entry absent from the command is inserted **right after `argv[0]`**; entries already present are left untouched. |

The rewrite adjusts the *command line*, not a path/env grant. It is gated by the
layer's `filter` (and any wrapping `[[policies]]`), so a layer with
`filter = { executables = ["claude"] }` only rewrites `claude` invocations and is a
no-op for everything else — put `rewrite` in a filtered layer so it never leaks onto
an unrelated command. Presence is an exact whole-argument match (`--flag` and
`--flag=value` are considered different); idempotent across repeated runs.

Typical use: a process is already confined by isol8, so you want the wrapped tool to
skip its *own* interactive permission prompts — e.g. inject
`--dangerously-skip-permissions` for Claude Code. This is **opt-in**, not a built-in
default; author it in your own layer (see
[`examples/profiles/claude-skip-permissions.toml`](../examples/profiles/claude-skip-permissions.toml)).

```toml
# load with: isol8 --profile-path ./my-rewrites.toml ...
filter = { executables = ["claude"] }
rewrite = { ensure_args = ["--dangerously-skip-permissions"] }
```

### NetworkPolicy — *Phase 3, not parsed yet (§0)*

| Field | Type | Default | Meaning |
|---|---|---|---|
| `tier` | enum `n0`\|`n1`\|`n2`\|`n3` | `n1` | Domain-filtering confinement tier (R5). `auto` (CLI) picks strongest supported, falling back N3→N2→N1→N0 (R5.7). |
| `allow_domains` | array of string | `[]` | Allowlisted hosts; glob `*` for one label (`*.githubusercontent.com`). Effective allowlist = union across enabled layers (R5.3). |
| `deny_domains` | array of string | `[]` | Explicit blocklist; wins over allow. |
| `inspect` | enum `hostname`\|`mitm` | `hostname` | SNI/CONNECT-host filtering vs full MITM (R5.2). |
| `sockets` | enum `none`\|`outbound`\|`localhost`\|`all` | `all` | Socket-class grant, distinct from the domain `tier` (Seatbelt `network*` vs `network-outbound` vs localhost-only). **macOS-only**; Linux uses tier/proxy. |

---

## 6. Merge semantics (deny-first)

Layers fold in resolved order (§3, ties by band §2). The model is **additive**:
each layer only adds; conflicts resolve by **highest-layer-explicit-grant-wins**.
This matches the Safehouse rule "later modules add allows; revoke only by appending
an explicit deny" — an appended deny is just a top-layer `none` grant.

- **paths** — keyed by normalized path (after `~` expansion against the effective
  home) **and** `match` kind. Per key, the **highest (most-recent) layer that sets
  an explicit grant wins** — including `none`. There is no unconditional "deny
  always wins": a top-layer re-grant can override a lower-layer `none`, and a
  lower-layer `none` cannot revoke a higher-layer allow. A child path refines a
  parent: `~ = rw` + `~/.ssh = none` ⇒ `~/.ssh` denied, rest of home rw (the more
  specific key has its own winner).
- **env** — union; **first writer wins** (lower layers are defaults), so a toolchain
  layer does not clobber a base default. The `--env` full-inherit escape hatch
  (R3.4) bypasses this and passes the host env through.
- **home_replace** — taken from the **highest** layer that sets it; `seed` lists are
  unioned across layers.
- **rewrite** — `ensure_args` are **unioned** across layers (deduped, first-seen
  order). The merged list is applied to the command after the merge, inserting any
  missing args after `argv[0]`.
- **network** — `tier` = strongest requested (or `auto`); `allow_domains` = union;
  `deny_domains` = union and wins over allow; `inspect` = strongest (mitm > hostname)
  if any layer requests it; `sockets` = strongest requested.
- **macos** — capability sets unioned; raw SBPL blocks concatenated in layer order
  (§8).

Invocation overrides enter as the top layer, so they win under these same rules.

---

## 7. Path expansion & the HOME-first rule

`~` and `$HOME`-relative paths expand against the **effective** home, which is
resolved **before** any merge (R4.2). By default the effective home is the **real**
home, so a layer written as `~/.cargo` targets the real `~/.cargo`. When a
replacement home is active (`--home`/`home_replace`), the same layer instead targets
the replacement — collapsing a large class of grants into one decision and keeping
the real dotfiles untouched (R4.5/R4.6).

To grant a **real-home** path even while a replacement home is active, use the
`#HOME` token: `#HOME` is substituted with the real home *before* `~` expansion, so
`{ path = "#HOME/.ssh", access = "ro" }` reaches the real `~/.ssh` regardless of
`--home`. With no replacement home, `#HOME` and `~` coincide. Works in profile
grants and in `--add-dirs-*` overrides.

Order guarantee:

```
select_layer_names → resolve_requires → apply_layer_filter (per layer)
  → home::resolve → expand ~ → merge (+ CLI overrides) → backend render
```

`home::resolve` reads `home_replace` from the **filtered** layer stack, so a
layer skipped by OS filter does not contribute home policy.

---

## 8. macOS rule-vocabulary extension (`macos`)

The portable model above (paths/env/network) is the cross-platform lowest common
denominator. The upstream Agent Safehouse profiles also grant macOS/Seatbelt
operation classes that **have no Linux/Landlock equivalent** (they concern mach
ports, IOKit, user preferences, etc. that don't exist on Linux). These live in an
OS-scoped `macos` block, applied only by the Seatbelt backend; the Linux backend
ignores them with a documented warning.

```toml
[profile.keychain.macos]
capabilities = ["mach-lookup", "ipc-posix-shm"]
raw = """
(allow file-read* file-write* (home-subpath "/Library/Keychains"))
(allow mach-lookup (global-name "com.apple.SecurityServer"))
"""
```

| Field | Type | Meaning |
|---|---|---|
| `capabilities` | array of enum | Typed common classes: `mach-lookup`, `mach-register`, `iokit-open`, `sysctl-read`, `process-exec`, `process-fork`, `process-info`, `signal`, `pseudo-tty`, `user-preference-read`, `user-preference-write`, `ipc-posix-shm`, `sysv-sem`, `pasteboard`. |
| `raw` | string (SBPL) | Verbatim Seatbelt rules for the long tail (specific `global-name`s, `iokit-user-client-class`, regex matchers). Concatenated after generated rules. |

**Why both.** Typed `capabilities` keep the common cases auditable and renderable;
`raw` is the escape hatch so a profile never has to wait on isol8 to model a new
operation class. Per the feasibility review, ~70% of the sample's rule *content* is
this macOS-only vocabulary — fully expressible via this block, inherently N/A on
Linux. General user-defined SBPL macros (`define_functions`) are out of scope; the
three home macros are covered by `match` + `~`-expansion (§5/§7).

---

## 9. Validation rules

- `access` ∈ `none|ro|rw|metadata`; `match` ∈ `subpath|literal|prefix|regex`;
  `tier` ∈ `n0|n1|n2|n3`; `inspect` ∈ `hostname|mitm`; `sockets` ∈
  `none|outbound|localhost|all` — unknown values are a load error naming file+layer.
- `path` must be absolute or `~`-prefixed; relative paths are rejected.
- `requires` referencing an unknown layer, or forming a cycle, is a hard error
  (the cycle path is reported, §3).
- Unknown fields are rejected (`#[serde(deny_unknown_fields)]`) to catch typos —
  a silently-ignored grant is a security footgun.
- A `macos` block on a non-macOS run is loaded but ignored, with a warning.
- `--show-policies` / `--dry-run` render the layer stack and fully merged effective
  policy so the model is auditable before any process starts.
- `--show-profiles` (no command) or `isol8 @profiles-list` shows every known layer
  and its source (`Builtin`, `UserConfig`, `ProfilePath(path)`).
- `--show-profiles CMD...` shows which layers were selected (defaults, explicit,
  auto-match) for that command.
- `profile_paths` / `--profile-path` must exist; a typo is a load error, not a
  silent widening of policy.

---

## 10. Built-in layer inventory (Safehouse port)

The embedded tree mirrors [Agent Safehouse](https://github.com/eugene1g/agent-safehouse)
composition bands (generalized cross-platform):

| Band | Path prefix | Examples |
|------|-------------|----------|
| 00 base | `base` | deny-by-default baseline, real HOME (replacement opt-in) |
| 10 system-runtime | `macos/system-runtime`, `linux/system-runtime` | OS essentials; aliases `macos-system`, `linux-system` |
| 20 network | `network` | requires-only stub until Phase 3 |
| 30 toolchains | `toolchains/*` | `rust`, `node`, `python`, … |
| 40 shared | `shared/*` | `agent-common`, `ipc-sysv-sem` |
| 50–55 integrations | `integrations/*` | `git`, `keychain`, `macos-gui`, `electron`, … |
| 60 agents | `agents/*` | `claude-code` (auto: `claude`), `codex`, … |
| 65 apps | `apps/*` | `claude-app`, `vscode-app`, … |
| Linux-only | `linux/*` | `secret-service`, `gui` |

macOS-specific Seatbelt rules live in `[macos]` blocks (`capabilities` + `raw` using
TOML literal strings `'''…'''` for regex-heavy SBPL). Linux counterparts use path
grants and OS filters where no Landlock/Mach equivalent exists.
