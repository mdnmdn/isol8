# isol8 — Target Project Structure & Code Blueprint

> Layout of the `isol8` **Cargo workspace** (Phase 9), with module responsibilities,
> key types, and data flow. macOS + Linux MVP enforced; evolution Phases 0–9 done
> (see [`AGENTS.md`](../AGENTS.md)). Notes on consolidation still true inside each crate:
>
> - `profile` is a single module (types + `LayerRegistry` + load + merge +
>   `resolve_requires` + `select_layer_names`), not a submodule dir.
> - `isol8-core` `build.rs` walks workspace-root `profiles/**/*.toml` and
>   `recipes/**/*.toml` and emits embedded tables at compile time.
> - Typed errors (`error`) and the library surface (`sandbox`: `Spec`, `Sandbox`,
>   `SandboxChild`, `DryRun`) live in **isol8-core**.
> - `--dry-run` / `--show-policies` render via `print_dry_run(&DryRun)` in the CLI
>   crate; `Backend::spawn` returns `Result<SandboxChild>` (non-blocking).
> - Root package `isol8` is a **facade**: re-exports core (+ optional registry/cli);
>   binary is `src/bin_shim.rs` → `ensure_registry_provider()` + `isol8_cli::cli::main()`.
> - Integration tests under workspace-root `tests/` depend on the facade.
> - `net/`, `caps.rs`, and the N3 helper are still future.
> - **Evolution track:** Phases **0–9 done** (cages → wizard → **crate split**).
>   Next: Linux shadow observe (Phase 10, deferred). Sequencing:
>   [`wip/multi-evo-plan.md`](./wip/multi-evo-plan.md) (design:
>   [`inbox/evo-repo.md`](./inbox/evo-repo.md)).
>
> Companion to the requirements in [`project-description.md`](./project-description.md).
> Section refs (R1–R6, N0–N3, §7) point there.

---

## 1. Workspace layout (Phase 9)

```
isol8/                              # Cargo workspace (resolver = "2")
├── Cargo.toml                      # [workspace] members + package `isol8` facade
│                                   #   features: default = [cli, registry]
│                                   #   registry → isol8-registry
│                                   #   cli → registry + isol8-cli
│                                   #   field-test → field-test bin
├── src/
│   ├── lib.rs                      # facade re-exports (isol8_core + optional registry/cli)
│   │                               #   ensure_registry_provider() wires offline recipe dirs
│   ├── bin_shim.rs                 # [[bin]] isol8  (required-features = ["cli"])
│   └── field_test_shim.rs          # optional path for field-test wiring
├── crates/
│   ├── isol8-core/                 # engine (no registry I/O, no CLI)
│   │   ├── Cargo.toml              # package isol8-core 0.2.6
│   │   ├── build.rs                # embeds ../../profiles + ../../recipes
│   │   └── src/
│   │       ├── lib.rs              # pub mods + type re-exports
│   │       ├── error.rs            # Error, Result, ResultExt
│   │       ├── sandbox.rs          # Spec, Sandbox, SandboxChild, DryRun
│   │       ├── profile.rs          # Profile, Policy, LayerRegistry, merge, resolve_requires
│   │       ├── filter.rs           # ProfileFilter matching, policies fold
│   │       ├── resolve.rs          # effective_policy(&Spec)
│   │       ├── env.rs              # sanitized environment (HOME first)
│   │       ├── home.rs             # R4 effective-home resolution
│   │       ├── context.rs          # Context, Platform, managed_root
│   │       ├── plan.rs             # HomePlan plan/apply
│   │       ├── cage.rs             # Cage selection → Spec overlay
│   │       ├── recipe.rs           # Recipe registry + strategy compile;
│   │       │                       #   path_prepend globs, per-source override;
│   │       │                       #   set_offline_registry_provider hook
│   │       ├── detect.rs           # @cage detect / verify + commands_trusted
│   │       ├── analyze.rs          # shared --analyze denial → recipe suggestions
│   │       ├── analyze_macos.rs    # macOS unified-log scrape + --author
│   │       └── backends/
│   │           ├── mod.rs          # Backend trait, select()
│   │           ├── linux.rs        # Landlock + no_new_privs
│   │           ├── macos.rs        # Seatbelt + sandbox-exec
│   │           └── windows.rs      # AppContainer draft
│   ├── isol8-registry/             # offline registries (depends on isol8-core only)
│   │   ├── Cargo.toml
│   │   └── src/lib.rs              # ProfileSource, DirSource, lockfile, cache, trust
│   └── isol8-cli/                  # CLI library (depends on core + registry)
│       ├── Cargo.toml              # clap, serde_yaml, dialoguer, toml_edit, anyhow
│       └── src/
│           ├── lib.rs              # pub mod cli, wizard
│           ├── wizard.rs           # @cage new/edit managed sections, drift, bundles
│           ├── cli/
│           │   ├── mod.rs          # pub fn main(); run + meta commands
│           │   ├── config.rs       # isol8.toml/yaml, markers, ISOL8_*, [registries.*]
│           │   │                   # see _docs/config.md
│           │   └── diag.rs         # @diag (macOS)
│           └── bin/
│               └── isol8-field-test.rs
├── profiles/                       # built-in TOML layers (~70); embedded by isol8-core
│   ├── base.toml
│   ├── macos-system.toml           # backward-compat alias → macos/system-runtime
│   ├── linux-system.toml
│   ├── macos/system-runtime.toml
│   ├── linux/system-runtime.toml
│   ├── toolchains/rust.toml
│   ├── integrations/git.toml
│   ├── agents/claude-code.toml
│   └── …
├── recipes/                        # built-in recipes; embedded by isol8-core
│   └── toolchains/{nvm,cargo,maven}.toml
├── tests/                          # integration tests against facade package `isol8`
│   ├── profile_merge.rs
│   ├── profile_path.rs
│   ├── cage.rs
│   ├── recipe.rs
│   ├── registry.rs
│   ├── wizard.rs
│   ├── analyze.rs
│   └── detect_verify.rs
├── AGENTS.md
└── _docs/
    ├── project-description.md
    ├── profile-model.md
    ├── project-structure.md        # this file
    └── …
```

**Dependency graph (no cycles):**

```
isol8-core
    ↑
isol8-registry ──→ isol8-core
    ↑
isol8-cli ───────→ isol8-core + isol8-registry
    ↑
isol8 (facade) ──→ isol8-core + optional registry + optional cli
```

**Embeddable.** Prefer the facade package:

| Dependency | Features | What you get |
|------------|----------|--------------|
| `isol8` | default (`cli` + `registry`) | Full surface (same as binary stack) |
| `isol8` | `default-features = false` | Engine only (`isol8-core` re-exports) |
| `isol8` | `default-features = false, features = ["registry"]` | Engine + offline registry types; call `ensure_registry_provider()` if using config-backed recipe dirs |
| `isol8-core` | path/version | Direct engine crate (no facade) |

CLI (clap + serde_yaml + dialoguer + wizard) is behind feature `cli`. Policy logic
never lives in the CLI crate.

**Binaries.** `isol8` (root, unprivileged, `required-features = ["cli"]`) and
`isol8-field-test` (`field-test` feature). Future: `isol8-net-helper` (Phase 3,
file-capability `cap_net_admin+ep`). The main binary never needs root.

---

## 2. Data flow (one `isol8 <cmd>` invocation)

```
cli::Cli::parse()  [feature = "cli"]
   │  ProfileOpts { cage, profiles, profile_paths, auto_profiles, add_dirs_rw/ro, home, … }
   ▼
cli::config::load()                      ── ISOL8_CONFIG_PATH | project markers
   │  Config { default_profiles, auto_profiles, profile_paths, cage, … }   (config_path / merge)
   │                                           | ~/.config/isol8/
   ▼
cage::resolve + apply_cage_to_opts()     ── --cage / ISOL8_CAGE / config cage / .isol8 discovery
   │  fills empty opts fields only (CLI already set wins)
   ▼
config::apply_to_run() + apply_env_overrides()   ── remaining empties + ISOL8_* overrides
   ▼
ProfileOpts::into_spec() → Spec { …, ephemeral_home, cmd }
   ▼
resolve::effective_policy(&Spec)          ← also called directly by Sandbox::run/spawn/dry_run
   │
   ├─ profile::LayerRegistry::load(profile_paths)
   │     builtin (build.rs embed) → user config dir → profile-path overlays
   │
   ├─ filter::RunContext::from_cmd(&cmd)
   ├─ recipe::RecipeRegistry::load + compile_all(spec.toolchains)
   │     builtins → ~/.config/isol8/recipes → offline registry dirs
   │     (isol8_registry::discover_* via recipe provider hook; later source
   │      replaces an overlapping variant of the same id)
   │     → home_ops + path grants + env + path_prepend + layer `requires`
   ├─ profile::select_layer_names()        ── default_profiles + --profile + auto_profiles
   │     (executable filter match on layer.filter.executables)
   │     + recipe `requires` appended (provenance: required)
   ├─ profile::resolve_requires()           ── transitive requires, cycle detect, dedup
   ├─ filter::apply_layer_filter() per layer   ── skip grants when os/arch/executable mismatch;
   │     fold matching [[policies]] into layer
   ├─ Context::from_environment()          ── real_home, cwd, platform, managed_root
   │                                          (recipes already compiled above; no
   │                                           network, missing git caches skipped)
   ├─ home::resolve(&spec+recipe_ops, …)   ── R4: effective $HOME FIRST; @managed/<id>;
   │                                          HomePlan (mkdir + seed-ro + recipe/spec ops)
   ├─ profile::load_merged()               ── ~ + #HOME expansion, --add-dirs-* override layer, merge
   │                                          + recipe path grants + expanded recipe env
   ├─ env::build_minimal()                 ── R3.1 allowlist, HOME first, then --env-pass / --set-env
   └─ apply_path_prepend()                 ── recipe path_prepend → front of PATH
                                              (globs resolved through planned links)
   │  EffectivePolicy { layer_names, profile, env, home, recipes }
   ▼
backends::select()
   │
   ├── dry_run / --show-policies ?
   │     sandbox::dry_run(&Spec) → DryRun { …, home_plan, home_path }  (plan NOT applied)
   │     cli: print_dry_run(&DryRun) ; return
   ▼
home::materialize(&effective.home)       ── HomePlan::apply (seed-ro / link / mkdir / copy)
   ▼
resolve::confine_executable(&mut profile, &mut cmd)
                                         ── exec path only: resolve cmd[0] on host PATH
                                            (clean "command not found"), auto-grant the
                                            resolved binary ro so deny-by-default can't hide it
   ▼
backend.spawn(&profile, &env, &cmd)      ── apply OS policy, exec (non-blocking)
   │  SandboxChild  { id(), wait() -> Result<i32>, kill() -> Result<()> }
   ▼
child.wait() → i32 exit code
   ▼
std::process::exit(code)
```

Introspection (`--show-policies`, `--show-profiles`, `@profiles-list`, `@profiles-show`)
reuses `LayerRegistry`, `select_layer_names`, and `resolve::effective_policy` without
spawning — and *without* `confine_executable`, so policy can be inspected for a command
that is not installed (no "command not found", no auto exe-grant).

**Ordering invariant:** `home::resolve` runs *before* `profile::merge`, so every
`$HOME`-relative grant in every layer is computed against the effective home. By
default the effective home *is* the real home (HOME replacement is opt-in via `--home`
or a layer's `home_replace`); when replacement is on, no layer can compute a grant
against the real home (R4.2/R4.6).

---

## 3. Module blueprints

Paths below are relative to the crate that owns them. Via the facade they remain
available as `isol8::…` (with the same feature gates).

### `isol8-cli` — `cli/` (feature `cli` on facade)

No `run` subcommand — the confined command is passed directly. Meta/admin commands
use an `@` prefix (`cli::META_PREFIX`) so they never collide with the confined argv.

```rust
// crates/isol8-cli/src/cli/mod.rs  — pub fn main() -> anyhow::Result<()>

// Normal usage:
isol8 [ProfileOpts] <COMMAND> [ARGS]...

pub struct ProfileOpts {
    pub profiles: Vec<String>,        // --profile
    pub profile_paths: Vec<String>,   // --profile-path
    pub auto_profiles: bool,          // --auto-profiles
    pub add_dirs_rw/ro: Vec<String>,
    pub home: Option<String>,
    pub no_seed: bool,                // --no-seed (skip home seeding)
    pub env_pass: Vec<String>,        // --env-pass NAME
    pub set_env: Vec<String>,         // --set-env K=V
    pub show_policies: bool,          // --show-policies (alias: --dry-run)
    pub show_profiles: bool,          // --show-profiles (list or resolve)
    pub verbose: bool,
}

// Meta commands (never passed to the confined process):
isol8 @init [--path DIR] [--format toml|yaml]
isol8 @profiles-list [--verbose] [ProfileOpts]
isol8 @profiles-show <NAME> [ProfileOpts]

// Bare `isol8` → help.
```

`cli::parse()` returns `ParsedCli::{Help, Run, Init, ProfilesList, ProfilesShow}`.
CLI builds a `Spec` consumed by `resolve::effective_policy` (and `Sandbox` internals).
`print_dry_run(&DryRun)` renders the text report from the structured `DryRun` value.
`cli/config.rs` — global config discovery and `ISOL8_*` env overrides.
`cli/diag.rs` — `@diag` delta-debug helper (macOS only).
`wizard.rs` (same crate) — `@cage new`/`edit` managed sections, drift, bundles.

### `isol8-core` — `profile.rs` (drives everything)

Implemented as a single module (target `profile/` split is deferred). Key types:

```rust
pub enum Access { None, Ro, Rw, Metadata }

pub struct PathGrant { pub path: String, pub access: Access, pub r#match: MatchKind }

pub struct ProfileFilter { pub os: Vec<String>, pub arch: Vec<String>, pub executables: Vec<String> }

pub struct Policy { pub filter: ProfileFilter, pub paths: Vec<PathGrant>, pub macos: Option<MacosExtra> }

// One TOML layer as authored (also the merged result — ponytail: split if needed).
pub struct Profile {
    pub requires: Vec<String>,
    pub filter: Option<ProfileFilter>,   // layer-level: skip grants when no match
    pub policies: Vec<Policy>,           // conditional grant bundles
    pub paths: Vec<PathGrant>,
    pub env: HashMap<String, String>,
    pub home_replace: Option<HomeReplace>,
    pub macos: Option<MacosExtra>,
    // Phase 3: network: Option<NetworkPolicy>
}

pub enum LayerSource { Builtin, UserConfig, ProfilePath(String) }

pub struct LayerRegistry { /* HashMap<name, LayerEntry> */ }

pub fn select_layer_names(run, registry, ctx) -> Result<Vec<String>>;
pub fn resolve_requires(selected, all) -> Result<Vec<(String, Profile)>>;  // names kept for provenance
pub fn merge(layers) -> Profile;
pub fn load_merged(run, layers, home, ctx) -> Result<Profile>;
```

**Layer registry overlay** (lowest → highest priority on name collision):

1. Built-in — `build.rs` embed of `profiles/**/*.toml` (namespaced: `agents/claude-code`)
2. User config dir — `$XDG_CONFIG_HOME/isol8/profiles/**/*.toml` (silent skip if absent)
3. Profile paths — `--profile-path` / `config.profile_paths` (file or directory; hard error if missing)

**Selection** (`select_layer_names`): `default_profiles` (from config) ∪ explicit
`--profile` ∪ layers auto-selected when `auto_profiles` is on and
`filter.executables` matches the command basename. Then `resolve_requires` expands
deps; `filter::apply_layer_filter` strips non-matching grants (deps still pulled).

See [`profile-model.md`](./profile-model.md) for schema and merge rules.

### `isol8-cli` — `cli/config.rs`

```rust
pub struct Config {
    pub default_profiles: Vec<String>,  // e.g. ["base", "macos/system-runtime"]
    pub auto_profiles: bool,
    pub profile_paths: Vec<String>,
    pub add_dirs_rw: Vec<String>,
    pub add_dirs_ro: Vec<String>,
    pub home: Option<String>,
    pub dry_run: bool,
    // + cage, registries, …
}
```

Discovery: `ISOL8_CONFIG_PATH` (file or dir; no local merge) → project markers
(`isol8.toml` / `.isol8.toml` / `encage.toml` / `.encage.toml` with optional
`config_path`, `ignore_global`, field overlay) → `~/.config/isol8/isol8.toml`.
`@…` paths expand relative to the base config directory.
`ISOL8_PROFILE`, `ISOL8_PROFILE_PATH`, `ISOL8_ADD_DIRS_RW`, `ISOL8_HOME`,
`ISOL8_DRY_RUN`, etc. mirror CLI flags.

### `isol8-core` — `filter.rs`

`RunContext { cmd, os, arch }`, `filter_matches`, `apply_layer_filter`,
`apply_policies` (fold `[[policies]]` into unconditional fields when filter matches).

### `isol8-core` — `resolve.rs`

`effective_policy(&Spec) -> EffectivePolicy` — shared pipeline for `run`,
`--show-policies`, and `--dry-run`. `EffectivePolicy.layer_names` is the resolved
(deps-first) stack tagged with `LayerOrigin` (`Explicit` / `Auto` / `Required`) so
`--show-policies` shows *why* each layer contributes. `parse_set_env(&[String])`
parses `--set-env K=V` pairs (errors on a missing `=`, no silent drop) before
`env::build_minimal`. `confine_executable(&mut Profile, &mut [String])`
— called only on the exec paths (`run`, `@diag`): resolves `cmd[0]` execvp-style
against the host `PATH` to an absolute path (clean `command "x" not found` on miss)
and auto-grants the resolved binary `ro` so deny-by-default never hides the
command's own executable (e.g. an agent under `~/.local/bin`).

### `isol8-core` — `home.rs` — R4, first-class

```rust
pub struct EffectiveHome { pub path: PathBuf, pub seed: Vec<SeedEntry> }

/// CLI --home > profile home_replace (path | auto_scratch) > the REAL home.
/// HOME replacement is opt-in: with nothing requesting it, the real home is used.
/// Resolved before profile merge.
pub fn resolve(run: &RunArgs, layers: &[ProfileLayer]) -> Result<EffectiveHome>;

/// Copy allowlisted real-home entries read-only into the home (R4.4).
/// First-creation-only: an existing entry is left untouched (no re-copy, no error).
pub fn seed(home: &EffectiveHome) -> Result<()>;

/// Expand a grant path: substitute the `#HOME` real-home token, then expand a
/// leading `~` against the effective home. Used for profile + --add-dirs-* paths.
pub fn expand_grant(path: &str, effective_home: &Path) -> String;
```

`--no-seed` (a `RunArgs` flag) clears `EffectiveHome.seed` in `resolve`, so the run
seeds nothing regardless of profile seed lists.

### `isol8-core` — `env.rs` — R3

`build_minimal(&Profile, &Path, env_pass: &[String], set_env: &[(String,String)])
-> HashMap<String,String>`. Filters `std::env` to the allowlist
(`HOME, PATH, SHELL, TMPDIR, USER, LOGNAME, PWD`), applies the resolved HOME first,
folds profile env (no override), then applies CLI controls highest-precedence:
`--env-pass NAME` pulls a named host var through, `--set-env K=V` sets one
explicitly. The `ISOL8_SANDBOXED` marker is stamped last so `--set-env` can't clear
it. (`--env-file` is still future.)

### `isol8-registry` — offline sources (feature `registry` on facade)

`ProfileSource` trait, `DirSource`, `LayeredSource`, `Lockfile`, `TrustLevel`,
`open_offline`, `update_registry`, `discover_offline_recipe_dirs`. Wired into
core recipe loading only after `isol8::ensure_registry_provider()` (or the binary
shim) installs the provider. Core never depends on this crate.

### `isol8-core` — `backends/mod.rs`

```rust
pub trait Backend {
    /// Apply OS policy and exec the command. Returns immediately with a non-blocking handle.
    fn spawn(&self, profile: &Profile, env: &HashMap<String,String>, cmd: &[String]) -> Result<SandboxChild>;
    /// Render the OS-native policy text for the given profile (used by DryRun).
    fn render_policy(&self, profile: &Profile) -> String;
}

pub fn select() -> Box<dyn Backend>;     // cfg(target_os) dispatch

pub struct Caps { pub net_admin: bool, pub userns: bool, pub landlock_abi: Option<u32>, pub pasta: bool }
pub fn probe() -> Caps;                   // feeds R5.7 tier auto-select + error UX
```

- `backends/linux.rs` — `LinuxBackend`. Build Landlock `Ruleset` from `paths`
  (deny-by-default; `AccessFs` ro/rw via `PathBeneath`), set `PR_SET_NO_NEW_PRIVS`,
  optionally enter user+mount namespaces to bind the replacement home over the real
  home (R4.6) and for ancestor-metadata correctness (R2.3). `restrict_self()`, then
  hand off to `spawn.rs`. Resource limits (R1.3) via `setrlimit`/cgroups here.
- `backends/macos.rs` — `MacosBackend`. Generate Seatbelt policy text
  (`(deny default)`, `(allow file-read* (subpath …))`, `(allow file-write* …)`,
  metadata via `file-read-metadata`) and invoke `/usr/bin/sandbox-exec -p <policy>`.
- `backends/windows.rs` — `WindowsBackend` (Phase 5). AppContainer SID + per-object
  ACLs, Job Objects for limits, env block construction. Stubbed until then.

### `net/` — R5 (Phase 3)

- `net/mod.rs` — `NetTier { N0, N1, N2, N3 }`, tier auto-select with graceful
  fallback N3→N2→N1→N0 (R5.7) using `caps::probe`.
- `net/proxy.rs` — N1 cooperative filtering proxy (hostname/SNI default; optional
  MITM with generated CA + per-toolchain env injection: `NODE_EXTRA_CA_CERTS`,
  `REQUESTS_CA_BUNDLE`, `GIT_SSL_CAINFO`, …). Domain allow/deny from profile layers.
- `net/pasta.rs` — N2: unshare net ns, spawn `pasta` pointed only at the proxy.
- `net/helper.rs` — N3 client: drive `isol8-net-helper`.

### `spawn` (target; logic lives in backends today)

Cross-platform child exec with policy applied is currently inside each
`Backend::spawn` implementation rather than a separate `spawn.rs` module.

### `caps.rs` (future, Phase 3)

Capability probing/dropping via `caps`/`nix`. Used by `backends::probe`, the net
tier selector, and the N3 helper (drop privilege before exec, R5.6).

### `isol8-net-helper` bin (future, Phase 3)

Standalone privileged helper. Creates gateway netns + veth, installs
nftables `tproxy`/`redirect`, starts the proxy, drops `CAP_NET_ADMIN`, execs the
main sandboxed process into the prepared namespace.

---

## 4. Invariants enforced structurally

- **HOME before grants.** `home::resolve` is called before `profile::merge`; merge
  takes `EffectiveHome` so grants resolve against the effective home. HOME replacement
  is opt-in (`--home`/`home_replace`); when on, no layer can compute a grant against
  the real home.
- **The command's own binary is reachable.** On the exec path, `confine_executable`
  resolves `cmd[0]` and auto-grants it `ro`, so deny-by-default never makes a command
  unrunnable just because its binary sits outside the granted trees.
- **Deny-by-default.** `Access::None` is the implicit default; backends start from a
  closed policy and only open what the merged `Profile` lists.
- **Unprivileged main.** Only `isol8-net-helper` holds a file capability; the
  main binary never escalates.
- **Single binary, no daemons.** No persistent state; scratch homes are temp dirs
  cleaned on exit.
- **Trust via transparency.** `--dry-run` / `--show-policies` render the layer
  stack and exact effective policy; `isol8 profiles resolve` shows which layers matched.
- **Config precedence.** Built-in defaults < config file < `ISOL8_*` env < CLI flags.
- **Profile-path overlay.** External dirs/files override same-named built-in layers;
  missing profile-path entries are hard errors (unlike the optional user config dir).

---

## 5. Build targets per phase

| Phase | Modules that become real |
|---|---|
| 1 | `cli`, `profile`, `config`, `filter`, `resolve`, `build.rs`, `env`, `home`, `backends/{linux,macos}` (MVP) |
| 2 | full `env` flags, R1.3 limits in `linux`, structured JSON policy dump, WSL2 paths |
| 3 | `net/*`, `caps`, `isol8-net-helper` bin |
| 4 | seccomp in `linux`, JSON export in `render`, `tests/integration_*` |
| 5 | `backends/windows` (full path enforcement) |
| Evo 1–8 | cages, Context/HomePlan, recipes, detect/verify, analyze, registry, wizard |
| Evo 9 | workspace: `isol8-core` / `isol8-registry` / `isol8-cli` + root facade (done) |
| Evo 10 | Linux `--analyze` shadow mode (deferred) |
