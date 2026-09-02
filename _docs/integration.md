# isol8 — Host integration guide

How to embed isol8 inside a **host that runs other programs**: an agent manager,
a task harness, a CI runner, an IDE backend. The host owns sessions, workspaces
and lifetimes; isol8 owns the policy and the confined process.

[`embedding.md`](./embedding.md) is the API reference — feature matrix, per-call
signatures, `--json`, error types. **This guide is the architecture**: which
surfaces you extend, which you inject, which are closed, and how to drive the
whole thing from host state instead of from the user's dotfiles.

Worked, compiled version of everything here:
[`examples/embed_harness.rs`](../examples/embed_harness.rs) (`cargo run --example embed_harness`).

---

## 1. Division of labour

| The host owns | isol8 owns |
|---|---|
| Session identity, workspace, lifetime | The merged policy for one command |
| Where state lives (config dir, homes, caches) | Resolving `$HOME`, grants, env |
| Which profiles/recipes a session may use | Deny-first merge and OS enforcement |
| Approving new registry content | Rendering + applying the OS policy |
| Killing, restarting, cleaning up | Spawning and reporting the child |

The unit of work is a `Spec` — *what to confine and how*. Everything below is a
way of filling one in without inheriting the developer's machine.

```
host session ──▶ Context      (real_home, cwd, platform, config_dir, managed_root)
             ──▶ Config       (defaults the host chooses, not a discovered file)
             ──▶ Spec         (profiles + grants + home + home_ops + toolchains)
                    │
                    ├─ dry_run_in(&spec, &ctx)   → audit / show the user / store
                    └─ Sandbox::from_spec(spec).spawn(cmd)
                                                 → home materialized, policy applied,
                                                   SandboxChild { id, wait, kill }
```

---

## 2. Extension points

Be precise about what is extensible — a sandbox with too many seams is not a
sandbox.

| Surface | Kind | Do you implement it? | Where it plugs in |
|---|---|---|---|
| [`Backend`](#21-backend--closed-by-design) | trait | **No — closed** | `backends::select()` picks the OS backend |
| [`ProfileSource`](#22-profilesource--the-trait-you-may-implement) | trait (`Send + Sync`) | **Yes** | catalog: list / inspect / install artifacts |
| [`OfflineRegistryProvider`](#23-the-recipe-directory-hook) | `fn` pointer, process-global | Install once (or bypass) | where recipes are discovered |
| `Context` | struct you construct | Injected, not implemented | ambient state for the whole pipeline |
| Profiles / recipes / cages | TOML data | Authored, not coded | the actual policy extension mechanism |

### 2.1 `Backend` — closed by design

`Backend` (`isol8-core::backends`) is public and object-safe, but you **cannot**
implement it outside the crate: `spawn` must return a `SandboxChild`, whose
constructors are `pub(crate)`. That is deliberate — a pluggable enforcement layer
would let a host silently substitute a no-op sandbox.

```rust
let backend = isol8::backends::select();          // Linux → Landlock, macOS → Seatbelt
let text = backend.render_policy(&effective.profile);   // side-effect free
```

Use it to *render* and *inspect*. To enforce differently, change the profile, not
the backend.

### 2.2 `ProfileSource` — the trait you may implement

```rust
pub trait ProfileSource: Send + Sync {
    fn name(&self) -> &str;
    fn index(&self) -> &RegistryIndex;
    fn trust(&self) -> TrustLevel;
    fn root(&self) -> Option<&Path>;
    fn get_recipe(&self, id: &str) -> Result<Option<Recipe>>;
    fn get_profile(&self, id: &str) -> Result<Option<Profile>>;
}
```

Implement it when your host has its own catalog of recipes/profiles — an internal
service, a database, an OCI artifact, content embedded in your own binary.

**Know exactly what it is.** `ProfileSource` is a *catalog* trait: discovery,
inspection, trust and install UX. It is **not** consumed by the resolve pipeline.
Nothing in `resolve::effective_policy_in` asks a `ProfileSource` for anything.
Recipes and profiles reach a run through **directories on disk**:

| Artifact | How it reaches a run |
|---|---|
| Profile layer | `Spec::profile_paths` (dir or single `.toml`) + the layer name in `Spec::profiles` |
| Recipe | `Spec::recipe_paths`, or a registry dir passed to `RecipeRegistry::load_with_registry_dirs` |

So the integration shape is: **browse with the trait, then write what you accept
into a directory the engine reads.** That is exactly what `isol8 @registry
install` does, and it is why approval has a natural checkpoint (see §5.3).

Reuse over re-implementation: `DirSource` already implements the trait for
`registry.toml` + `index.json` on disk, and `LayeredSource::new(Vec<Box<dyn
ProfileSource>>)` composes any mix of sources with later-wins lookup — including
yours:

```rust
use isol8::registry::{DirSource, LayeredSource, ProfileSource};

let sources: Vec<Box<dyn ProfileSource>> = vec![
    Box::new(DirSource::open("official", "/opt/isol8/official")?),
    Box::new(my_internal_catalog),                       // your impl
];
let catalog = LayeredSource::new(sources);               // itself a ProfileSource
let recipe = catalog.get_recipe("toolchains/nvm")?;
```

### 2.3 The recipe-directory hook

Core must not depend on the registry crate, so offline recipe discovery is a
function-pointer hook:

```rust
pub type OfflineRegistryProvider = fn() -> Vec<(String, PathBuf)>;
pub fn set_offline_registry_provider(f: OfflineRegistryProvider);
```

`isol8::ensure_registry_provider()` installs the standard one. It is
**process-global and first-call-wins** — two hosts inside one process cannot
disagree, and the second call is a silent no-op.

If your host must not depend on process-global state (a plugin, a test suite, two
tenants in one process), **skip the hook entirely** and pass directories per
instance:

```rust
let dirs = vec![("internal".to_string(), PathBuf::from("/srv/isol8/recipes"))];
let reg = isol8::RecipeRegistry::load_with_registry_dirs(&[], &dirs)?;
```

That is the same call the hook feeds, with the ambient discovery removed. Build
the `RecipeRegistry` once per host and reuse it — it is the cache.

---

## 3. Configuration and profiles

### 3.1 Own the `Config`, don't discover one

`Config` is a plain struct in `isol8-core`. A harness should construct it rather
than let isol8 find a file:

```rust
let mut cfg = isol8::Config::builtin_defaults();   // base + this OS's system runtime
cfg.auto_profiles   = true;                        // match agents/* by executable name
cfg.profile_paths   = vec![state_dir.join("profiles").display().to_string()];
cfg.add_dirs_ro     = vec!["/opt/shared-cache".into()];
```

| Call | Reads | Use when |
|---|---|---|
| `Config::builtin_defaults()` | nothing | the host is the source of truth *(recommended)* |
| `config::load_in(&ctx)` | files under `ctx.config_dir` + a marker in `ctx.cwd` | you want project `isol8.toml` honoured, hermetically |
| `config::load()` | those **plus** `ISOL8_CONFIG_PATH`, real cwd | you are reproducing the CLI |
| `config::apply_env_overrides(&mut cfg)` | `ISOL8_*` | only if the user's env should influence sessions |

For a harness, `ISOL8_*` normally belongs to the *host* process, not to the
session — leave `apply_env_overrides` out unless you mean it.

### 3.2 Precedence, and the one trap

`resolve::spec_from_config(&cfg, base, cmd, &ctx)` applies the documented chain
([config.md §7](./config.md)): **builtin → config (+ marker) → cage → fields you
pre-set on `base`**. Pre-set fields always win.

The trap: the fill is **per field and all-or-nothing**. A non-empty `Spec` field
suppresses the config's value for that field entirely — it does not merge.

```rust
// WRONG — silently drops `base` and the OS system-runtime layer:
base.profiles = vec![session_layer];

// RIGHT — the session layer joins the host defaults:
cfg.default_profiles.push(session_layer);
```

Same for `add_dirs_rw`, `add_dirs_ro`, `profile_paths`, `home`. Decide per field
whether you are *replacing* (set it on the `Spec`) or *adding* (append to the
`Config`).

### 3.3 Injecting a profile the host generated

Profiles are data. To add per-session policy, build a `Profile` in memory,
serialize it, and drop it where `profile_paths` points:

```rust
use isol8::profile::{Access, MatchKind, PathGrant, Profile};

let layer = Profile {
    paths: vec![PathGrant {
        path: workspace.display().to_string(),
        access: Access::Rw,                 // None | Ro | Rw | Metadata
        r#match: MatchKind::Subpath,        // Subpath | Literal | Prefix | Regex
    }],
    env: HashMap::from([("ISOL8_SESSION".into(), id.clone())]),
    ..Default::default()
};

let dir = state_dir.join("profiles").join("harness");
std::fs::create_dir_all(&dir)?;
std::fs::write(dir.join(format!("{id}.toml")), isol8::profile::format_layer(&layer)?)?;
// Layer name = path under the profile root, minus `.toml`:  "harness/<id>"
```

Then reference it by that name in `profiles` / `default_profiles`.

Rules worth internalizing:

- **The name comes from the path.** `<root>/harness/s1.toml` → `harness/s1`;
  a single-file `profile_path` uses the file stem. Same-named layers from a later
  `profile_path` override earlier ones.
- **Merge is deny-first.** Layers can only make the policy *stricter* in
  aggregate: an `Access::None` grant anywhere wins over an `Rw` elsewhere. You
  cannot write a layer that re-opens what another layer denied — carve grants
  narrowly instead.
- **Tokens are expanded late:** `~` = effective home, `#HOME` = the real home
  (survives HOME replacement), `@…` = config dir, `@managed/<id>` = managed root.
- **`requires`** pulls other layers in transitively (tagged `required` in the
  reported stack); `filter` gates a layer by executable / OS / arch.
- Layers may also carry `env` defaults, `home_replace`, argv `rewrite`, and
  macOS `capabilities` — see [profile-model.md](./profile-model.md).

Inspect what actually got selected with `LayerRegistry`:

```rust
let reg = isol8::profile::LayerRegistry::load(&spec.profile_paths)?;
for (name, source) in reg.list() { println!("{name} ({source:?})"); }
```

---

## 4. Homes created programmatically

A per-session `$HOME` is usually the single highest-value thing a harness gets
from isol8: the agent's dotfiles, caches and credentials stop being the user's.

### 4.1 Choose the home

| `Spec` field | Effect |
|---|---|
| `home = Some("@managed/<id>")` | `{ctx.managed_root}/<id>` — persistent, host-named *(recommended)* |
| `home = Some("/abs/path")` | exactly that directory |
| `ephemeral_home = true` | a temp scratch home for this run |
| unset | the real `$HOME` is inherited — **no** replacement |

`ctx.managed_home("s1")?` resolves the `@managed/` form yourself. The id must be
a **single path segment** (no `/`, `\`, `..`).

### 4.2 Describe the mutations, then apply them

Home materialization is a **plan** — computed as data, applied as a separate
step, and idempotent:

```rust
use isol8::{HomeOpSpec, HomePlan};

let ops = vec![
    HomeOpSpec::mkdir("~/.cache"),
    HomeOpSpec::link("#HOME/.nvm", "~/.nvm"),        // share the real toolchain
    HomeOpSpec::seed_ro("#HOME/.gitconfig", "~/.gitconfig"),
    HomeOpSpec::copy("/srv/templates/agentrc", "~/.agentrc"),
];

let plan = HomePlan::compute(&ops, &ctx, &home_path)?;
println!("{}", plan.render());        // show the user; nothing written yet
plan.apply()?;                        // idempotent: skip-exists / skip-missing
```

Each `PlannedOp` carries a `PlanAction` (`apply` / `skip-exists` /
`skip-missing`), so you can log or gate on it. `plan.apply_count()` is how many
ops would actually write.

### 4.3 Who applies it, and when

- Ops on `Spec::home_ops` (plus recipe-contributed ops and profile `seed` lists)
  are applied **automatically on `spawn` / `run`**. You do not need to call
  `apply` yourself for the normal path.
- `dry_run_in` **never** touches disk — `dry.home_plan` is the preview.
- Call `HomePlan::apply` explicitly only to *pre-provision* a session home before
  the first run (warm it while the user is still choosing, say).
- Seeding is **first-creation only**; `Spec::no_seed = true` skips profile seed
  lists entirely.
- **Cleanup is the host's job.** isol8 never deletes a managed home. Remove
  `{managed_root}/<id>` when you retire a session.

### 4.4 Let recipes fill the home

A recipe turns "the agent needs node" into grants + home ops + env, under a
strategy the host picks:

```rust
let choice = isol8::ToolchainChoice::new("nvm", "link")?;   // share | link | isolate
let c = reg.compile(&choice, &rc)?;
spec.home_ops.extend(c.home_ops);
spec.toolchains.push(choice);       // or let resolve compile it during the run
if let Some(danger) = &c.danger { warn(danger); }   // surface the strategy's risk note
```

`detect::detect_all(&reg, &rc, &real_home)` tells you what the machine actually
has, so the host can offer only what exists; `detect::verify_toolchains(&spec)`
runs each recipe's smoke test **inside** the sandbox and returns structured
`VerifyResult`s — a good post-provision health check.

---

## 5. Registries programmatically

Registries are offline-by-default sources of recipes/profiles. Everything the
`@registry` command does is available as library calls.

### 5.1 Where the pieces live

```rust
use isol8::registry::{
    default_cache_root, open_offline, update_registry, diff_index,
    apply_update_to_lockfile, registries_from_config, verify_lock_against_disk,
    DirSource, Lockfile, RegistrySpec, TrustLevel,
};
```

| Concern | Call |
|---|---|
| Declared registries | `registries_from_config(&cfg)` or `parse_registries_from_toml(body)` |
| Git cache root | `default_cache_root()` (or a host-chosen path) |
| Open without network | `open_offline(name, &spec, &cache_root, &lock)` → `DirSource` |
| Fetch / refresh (git CLI, network) | `update_registry(name, &spec, &cache_root)` → `UpdateResult` |
| Pins | `Lockfile::load(path)` / `.save(path)` / `.registry(name)` |
| Record a fetch | `apply_update_to_lockfile(&mut lock, &upd, &src)` |
| What changed | `diff_index(&lock, &src)` → `Vec<DiffItem>` |
| Cache vs. lock drift | `verify_lock_against_disk(&registries, &cache_root, &lock)` |
| Content integrity | `src.verify_content_hashes()`, `src.index_content_hash()` |

`RegistrySpec::Path` is the air-gapped shape (a directory you control, no
network). `RegistrySpec::Git` needs the `git` CLI and a populated cache — open it
offline afterwards. `RegistrySpec::Http` is **not implemented** and returns an
error.

### 5.2 The update → review → pin cycle

```rust
let registries = registries_from_config(&cfg)?;
let cache_root = default_cache_root();
let lock_path  = state_dir.join("isol8.lock");
let mut lock   = Lockfile::load(&lock_path)?;

for (name, spec) in &registries {
    let src = match open_offline(name, spec, &cache_root, &lock) {
        Ok(s) => s,
        Err(_) => {                                   // not cached yet
            let upd = update_registry(name, spec, &cache_root)?;   // network
            DirSource::open_with_trust(name, &upd.path, Some(upd.trust))?
        }
    };
    let changes = diff_index(&lock, &src)?;
    if approve(&changes) {                            // your policy — see §5.3
        let upd = /* build from src.index_content_hash() */;
        apply_update_to_lockfile(&mut lock, &upd, &src);
    }
}
lock.save(&lock_path)?;
```

Then point recipe loading at the accepted roots
(`RecipeRegistry::load_with_registry_dirs`, §2.3).

### 5.3 Make the admission gate explicit

`DiffItem { id, change, kind, summary, flags }` is designed for exactly this.
`change` is `added` / `changed` / `removed` / `same`, and `flags` calls out the
security-relevant deltas:

- `FORBIDDEN path …` — a grant isol8 refuses to normalize away
- `ceiling violation: rw outside home …` — writes beyond the home ceiling
- `new rw on real home via …` — the recipe wants the user's real `$HOME`
- `sensitive path in …` — credential-shaped locations

A harness should refuse (or require a human) on the first two rather than
auto-accepting upstream content. The CLI's `--strict` does the same thing.

### 5.4 Trust gates command execution

`TrustLevel` is `official` / `community` / `local` / `untrusted`.
`TrustLevel::commands_allowed()` and `detect::commands_trusted(source)` gate
whether a recipe's **detect/verify commands may run at all** — content from an
untrusted registry is data, never something you execute to probe the host. Set
trust per registry in config, and don't raise it to make a probe work.

---

## 6. Reusing what already exists

Before writing anything, check whether one of these covers it:

| You need | Reuse |
|---|---|
| A disk-backed catalog | `DirSource` (implements `ProfileSource`) |
| Several catalogs, later-wins | `LayeredSource` |
| The whole config → `Spec` chain | `resolve::spec_from_config` |
| Cage semantics (named bundles of home + profiles + dirs) | `cage::{select_name, resolve_in, apply_overlay, list_cages_in, write_new}` |
| Authoring a cage file without prompts | `wizard::{render, apply, check_drift, expand_bundle}` (feature `wizard`) |
| "What toolchains does this host have?" | `detect::detect_all` + `format_detect_table` |
| "Does the sandbox actually work for this session?" | `detect::verify_toolchains` |
| "Why was that denied?" | `analyze::run_and_analyze(&spec, &ctx)` |
| Capturing a confined run's output | `sandbox::run_captured(spec)` → `CapturedRun` |
| Rendering the OS policy for display | `backends::select().render_policy(&profile)` |
| A non-Rust component | the binary + `--json` ([embedding.md §5](./embedding.md#5-machine-readable-output)) |

**Cages are worth a second look for a harness**: a cage *is* a named session
profile (home mode + profiles + dirs + toolchains) that fills empty `Spec` fields
and nothing else. If your sessions are user-configurable, storing them as cage
files under `{state_dir}/cages/` gets you authoring, discovery, drift detection
and a CLI-compatible on-disk format for free — and the user can reproduce a
session with `isol8 -c <name> …` outside your harness.

---

## 7. Running the session

```rust
let mut child = isol8::Sandbox::from_spec(spec.clone()).spawn(spec.cmd.clone())?;
let pid  = child.id();
let code = child.wait()?;       // or child.kill()?
```

- **Exit codes are the child's own.** Pass them through.
- **Nesting is refused.** isol8 sets `ISOL8_SANDBOXED=1` (`env::SANDBOX_MARKER`)
  in the confined environment; a nested `spawn` fails with `Error::NestedSandbox`
  (macOS Seatbelt cannot nest). If your harness itself runs confined, sessions
  cannot be confined again — check `sandbox::ensure_not_nested()` at startup and
  report it clearly instead of failing per session.
- **Library paths never call `std::process::exit`** — that is confined to the
  CLI, so an in-process host stays in control.
- **`Sandbox` is a value, not a service.** No daemons, no background threads;
  concurrency is whatever your host does with the handles. `RecipeRegistry` and
  `LayerRegistry` are plain values — build once, share by reference.
- The executable in `cmd[0]` is resolved against the **host** `PATH` before
  spawning and auto-granted `ro`, so a typo fails as `command "x" not found`
  rather than as a confusing denial.

### 7.1 A session on a pseudo-terminal (unix)

An interactive agent harness needs a controlling terminal, and its geometry has to
reach the kernel or it redraws at the wrong size. Use the hermetic pty entry point
so the pane never inherits the host's own cwd or `HOME`:

```rust
let mut pty = isol8::sandbox::spawn_pty_in(&spec, &ctx, isol8::PtySize { cols, rows })?;
let reader  = pty.try_clone_reader()?;   // File → the host's pump
let writer  = pty.take_writer()?;        // File → keystrokes
// host SIGWINCH → pty.resize(PtySize { cols, rows })?
let code    = pty.child().wait()?;       // or pty.child().kill()? when the tab closes
```

- **One process per pane, no shim.** macOS `sandbox-exec` `execve`s in place and
  Linux forks exactly once, so `pty.child().id()` is the harness's own pid. Closing
  a tab kills the harness directly — nothing to forward signals through, nothing to
  orphan, and the exit code is the agent's, not a supervisor's.
- **A host that already owns a pty** (`portable-pty`, or its own `openpty`) passes
  the slave: `SandboxStdio::from_tty(slave)?` → `sandbox::spawn_with_stdio_in`.
  Note that `portable-pty` 0.9 cannot *adopt* a foreign master (`UnixMasterPty` and
  its fields are private, `PtySystem::openpty` always mints its own pair), which is
  why `PtyChild` carries reader / writer / `resize` itself — the same three calls
  `MasterPty` offers, so one host abstraction covers both the confined and the
  unconfined path.
- **The seam does not widen the policy**, with two exceptions that apply only when
  a controlling terminal is requested: `TERM` / `COLORTERM` pass through as
  *defaults* (a TUI harness with no `TERM` starts blank or refuses to run), and on
  macOS the `pseudo-tty` capability joins the rendered SBPL. Both are failures that
  look like the harness crashing rather than like denials, so neither is left to
  each host to remember.
- **Probe nesting once, at startup** (see the bullet above). A pty host is exactly
  where a per-pane `Error::NestedSandbox` would be most confusing.
- **Windows is not supported** — ConPTY is separate work, and a Windows pane
  enforces no path grants anyway (§8).

---

## 8. Known limits

State these to your users rather than discovering them in production.

- **`Sandbox::spawn` / `run` use the ambient `Context`.** `Sandbox::spawn` calls
  `Context::from_environment()` internally, so the *spawning* process's `HOME` and
  cwd are what a plain `spawn` sees. The hermetic `_in` variants cover
  `config::load_in`, `resolve::effective_policy_in`, `sandbox::dry_run_in` and —
  for a confined session — `sandbox::spawn_with_stdio_in` /
  `sandbox::spawn_pty_in` (§7.1). There is still no `spawn_in` for the plain
  inherited-stdio case: use `spawn_with_stdio_in` with
  `SandboxStdio::from_fds(...)`, or set the process cwd to the session workspace
  before spawning.
- **User-config overlays are still ambient.** Even on the `_in` path,
  `LayerRegistry` and `RecipeRegistry` pick up
  `$XDG_CONFIG_HOME/isol8/{profiles,recipes}` (else `$HOME/.config/isol8/…`) from
  the **process** environment; `Context::config_dir` does not redirect them. The
  invoking user's own layers can therefore join a session's stack. Built-ins and
  everything in `profile_paths` / `recipe_paths` are unaffected — carry the
  policy your sessions depend on there rather than assuming the directory is
  empty.
- **Windows enforces no path grants.** The AppContainer backend isolates the
  process; per-path ro/rw is documentary only. Do not present a Windows session
  as path-confined ([windows-support.md](./windows-support.md)).
- **Linux has no denial log.** Landlock emits nothing to scrape, so `--analyze`
  on Linux needs an NDJSON feed (`ISOL8_ANALYZE_FEED`); macOS scrapes the unified
  log. Denial observation is best-effort and non-exhaustive everywhere.
- **`ProfileSource` is not consumed by the resolve pipeline** (§2.2) — materialize
  what you accept to a directory.
- **HTTP registries are unimplemented**; use `path` or `git`.
- **Process-global, first-call-wins:** `ensure_registry_provider()` /
  `set_offline_registry_provider`. Bypass with
  `RecipeRegistry::load_with_registry_dirs` when that is unacceptable.

---

## 9. Integration checklist

1. Pick a **state dir** the host owns; build a `Context` from it per session
   (`config_dir`, `managed_root`, `cwd` = the session workspace).
2. Build a `Config` from `builtin_defaults()`; add host profile paths and shared
   read-only grants. Skip `apply_env_overrides` unless the user's env should leak in.
3. Generate a **per-session layer**, write it under a `profile_paths` root, and
   **append** its name to `cfg.default_profiles`.
4. Give the session a home: `home = Some("@managed/<id>")` plus `home_ops`.
5. `resolve::spec_from_config` → `Spec`; pre-set only what must override.
6. `sandbox::dry_run_in` → audit, log, or show the effective policy. Store it —
   it is the record of what the session was allowed to do.
7. `Sandbox::from_spec(spec).spawn(cmd)` → keep the `SandboxChild` for `wait` / `kill`.
   For an interactive pane use `sandbox::spawn_pty_in(&spec, &ctx, size)` instead
   and keep the `PtyChild` (§7.1).
8. Retire the session: remove `{managed_root}/<id>` and its generated layer.

Then read [embedding.md](./embedding.md) for the per-call details, and
[profile-model.md](./profile-model.md) before writing policy of your own.
