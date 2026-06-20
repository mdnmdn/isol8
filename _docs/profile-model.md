# isol8 — Profile Model

> The profile is the single source of truth for what a confined process may do.
> A run's effective policy is the **deny-first merge** of an ordered stack of
> profile layers (expanded over their inheritance graph) plus invocation
> overrides. This document defines the on-disk format (TOML primary, YAML
> accepted), inheritance, every field, and the merge semantics.
>
> Companion to [`project-description.md`](./project-description.md) (R2–R6) and
> [`project-structure.md`](./project-structure.md) (`profile/` module). Mirrors the
> upstream **Agent Safehouse** composition model (see §9), generalized cross-platform.

---

## 1. Concepts

- **Layer** — one named profile fragment (a TOML/YAML table). Contributes path
  grants, env defaults, a home-replacement policy, and network allowlist domains.
- **Inheritance** — a layer declares prerequisites with `requires` (alias
  `extends`); the set is expanded transitively before merging (§3).
- **Stack** — the ordered list of enabled layers (after inheritance expansion),
  resolved deny-first into one effective `Profile`.
- **Override** — values supplied at invocation (`--add-dirs-rw`, `--home`,
  `--enable`, …). Treated as the highest-priority layer.

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
--  workdir            cwd granted rw; ancestors metadata-only
--  custom (CLI)       --add-dirs-rw / --add-dirs-ro / --home / --env-*
--  appended           explicitly appended profiles
```

`--profile NAME` (repeatable) and `--enable NAME,…` select layers; their transitive
`requires` are pulled in automatically.

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

**Resolution algorithm** (`resolve_requires`, runs before `merge`):

1. Start from the explicitly selected layers (`--profile` / `--enable` / detected).
2. DFS over `requires`, collecting every transitive dependency.
3. **Cycle detection** — a back-edge is a hard error reporting the cycle path.
4. **Dedup** — each layer appears once, even if required via multiple paths
   (diamonds: `claude-app → electron → macos-gui` and `… → vscode → macos-gui`
   yield a single `macos-gui`).
5. **Topological order** — dependencies before dependents; ties broken by band
   number (§2), then declaration order. A required layer lands at the earliest
   position that satisfies all its dependents.

The output is an ordered `Vec<ProfileLayer>` fed straight into `merge` (§6).
Inheritance is purely a layer-ordering resolver in front of the merge pipeline —
it adds no new merge rule.

---

## 4. File format

TOML is primary (built-in `profiles/*.toml`, embedded at build time). User layers
live in the config dir (`$XDG_CONFIG_HOME/isol8/profiles/`, or the platform
equivalent). YAML is accepted for user layers and parses into the same model.

A file may define one or many layers. Two equivalent forms:

**One layer per file** (file stem = layer name):

```toml
# profiles/rust.toml  → layer "rust"
requires = ["system-runtime"]
paths = [ { path = "~/.cargo", access = "rw" }, { path = "~/.rustup", access = "ro" } ]
env   = { CARGO_TERM_COLOR = "always" }
```

**Multiple layers in one file** (explicit `[profile.<name>]`):

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

YAML equivalent of the `base` layer:

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
| `paths` | array of PathGrant | `[]` | no | Filesystem grants (R2). |
| `env` | map<string,string> | `{}` | no | Env defaults, merged without override (R3.5). |
| `home_replace` | HomeReplace | unset | no | HOME replacement policy (R4). |
| `network` | NetworkPolicy | unset | no | Network tier + domain allowlist (R5). |
| `macos` | MacosExtra | unset | no | macOS-only capability grants + raw SBPL passthrough (§8). |

Layer name comes from the file stem (one-per-file form) or the `[profile.<name>]`
table key. It is not a field inside the table.

### PathGrant

| Field | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | — | Absolute, or `~`-prefixed (expands to **effective** home, §7). |
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
| `seed` | array of string | `[]` | Real-home entries copied/bound **read-only** into the replacement (R4.4), e.g. `~/.gitconfig`, a scoped `~/.ssh` subset. |

When active, the real home is **not** granted by default (R4.5); re-add via an
explicit `paths` grant if needed. Resolution precedence: `--home` > layer
`home_replace.path` > `auto_scratch` temp dir. The home token (`~` / `$HOME`) is
isol8's equivalent of the Safehouse `HOME_DIR` placeholder.

### NetworkPolicy

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
- **network** — `tier` = strongest requested (or `auto`); `allow_domains` = union;
  `deny_domains` = union and wins over allow; `inspect` = strongest (mitm > hostname)
  if any layer requests it; `sockets` = strongest requested.
- **macos** — capability sets unioned; raw SBPL blocks concatenated in layer order
  (§8).

Invocation overrides enter as the top layer, so they win under these same rules.

---

## 7. Path expansion & the HOME-first rule

`~` and `$HOME`-relative paths expand against the **effective** home, which is
resolved **before** any merge (R4.2). So a layer written as `~/.cargo` targets the
scratch/replacement home when one is active — collapsing a large class of grants
into one decision and keeping the real dotfiles untouched (R4.5/R4.6).

Order guarantee: `home::resolve` → `resolve_requires` → expand `~` in all layers →
`merge` → backend render.

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
- `--dry-run` renders the fully merged effective policy (including inheritance
  expansion) so the model is auditable before any process starts.
