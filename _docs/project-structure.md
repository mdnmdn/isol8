# isol8 — Target Project Structure & Code Blueprint

> The intended layout of the full `isol8` crate once all phases land, with
> module responsibilities, key types, and data flow. This document is the
> *destination*; the current tree (Phase 1, macOS + Linux MVP working — see
> [`AGENTS.md`](../AGENTS.md)) deliberately consolidates some of it:
>
> - `profile/` is a single `src/profile.rs` (types + `LayerRegistry` + load + merge +
>   `resolve_requires` + `select_layer_names`), not a submodule dir; there is no
>   separate `profile/render.rs`.
> - `build.rs` walks `profiles/**/*.toml` and emits `profiles_embedded.rs` (~70
>   Safehouse-derived layers embedded at compile time).
> - `config.rs`, `filter.rs`, and `resolve.rs` are real: global config discovery,
>   conditional layer/policy filters, and the shared effective-policy pipeline.
> - `src/error.rs` and `src/sandbox.rs` are real: typed errors and the public library
>   entry surface (`Spec`, `Sandbox`, `SandboxChild`, `DryRun`).
> - `--dry-run` / `--show-policies` render via `print_dry_run(&DryRun)` in the CLI
>   layer; the text report was moved out of `backends::render_dry_run` (deleted).
>   `Backend::spawn` returns `Result<SandboxChild>` (non-blocking).
> - `cli.rs` is now `src/cli/{mod,config,diag}.rs`, gated behind the default-on `cli`
>   feature; `src/main.rs` is a thin shim calling `isol8::cli::main()`.
> - A `src/lib.rs` re-exports the public API; `tests/` share the crate.
> - `home.rs`, `env.rs`, `backends/{macos,linux}.rs`, and the `isol8-field-test` bin
>   are real; `net/`, `caps.rs`, and the N3 helper are still future.
>
> Companion to the requirements in [`project-description.md`](./project-description.md).
> Section refs (R1–R6, N0–N3, §7) point there.

---

## 1. Crate layout (target)

```
isol8/
├── Cargo.toml                  # workspace-less single crate; main bin (cli feature) + net helper bin
├── build.rs                    # walks profiles/**/*.toml → OUT_DIR/profiles_embedded.rs
├── AGENTS.md
├── _docs/
│   ├── project-description.md  # requirements + ecosystem research
│   ├── profile-model.md        # on-disk schema, merge, filters
│   └── project-structure.md    # this file
├── profiles/                   # built-in TOML layers (~70), embedded at build time
│   ├── base.toml
│   ├── macos-system.toml       # backward-compat alias → macos/system-runtime
│   ├── linux-system.toml       # backward-compat alias → linux/system-runtime
│   ├── macos/system-runtime.toml
│   ├── linux/system-runtime.toml
│   ├── toolchains/rust.toml
│   ├── integrations/git.toml
│   ├── agents/claude-code.toml
│   └── …                       # full Safehouse port (see profiles/)
├── src/
│   ├── main.rs                 # thin shim: fn main() -> anyhow::Result<()> { isol8::cli::main() }
│   ├── lib.rs                  # re-exports: Error, Result, Access, MatchKind, PathGrant, Profile,
│   │                           #   confine_executable, effective_policy, EffectivePolicy, LayerOrigin,
│   │                           #   Sandbox, Spec, SandboxChild, DryRun; #[cfg(feature="cli")] pub mod cli
│   ├── error.rs                # pub enum Error (thiserror), pub type Result<T>, ResultExt::ctx
│   ├── sandbox.rs              # Spec, Sandbox builder, SandboxChild, DryRun, ensure_not_nested()
│   ├── cli/
│   │   ├── mod.rs              # pub fn main(); command glue (run/init/profiles/policies)
│   │   │                       #   [feature = "cli"] — depends on clap + serde_yaml
│   │   ├── config.rs           # isol8.toml/yaml discovery, ISOL8_* overrides, init template
│   │   └── diag.rs             # @diag delta-debug helper (macOS)
│   ├── filter.rs               # ProfileFilter matching, apply_layer_filter, policies fold
│   ├── resolve.rs              # effective_policy(&Spec) shared by run + policies show
│   ├── profile.rs              # Profile, Policy, LayerRegistry, merge, resolve_requires
│   ├── env.rs                  # sanitized environment construction (HOME first)
│   ├── home.rs                 # R4 effective-home resolution + seeding
│   ├── spawn.rs                # (target) cross-platform child exec — not split out yet
│   ├── backends/
│   │   ├── mod.rs              # Backend trait (spawn→SandboxChild, render_policy), select()
│   │   ├── linux.rs            # Landlock ruleset, PR_SET_NO_NEW_PRIVS, waitpid-based SandboxChild
│   │   ├── macos.rs            # Seatbelt policy text + sandbox-exec, Child-based SandboxChild
│   │   └── windows.rs          # AppContainer + Job Objects (Phase 5, stub)
│   ├── net/                    # (Phase 3, not started)
│   └── caps.rs                 # (Phase 3, not started)
├── src/bin/
│   ├── isol8-net-helper.rs     # (Phase 3) privileged N3 helper
│   └── isol8-field-test.rs     # real-sandbox field tests
└── tests/
    ├── profile_merge.rs        # deny-first merge + inheritance
    ├── profile_path.rs         # profile-path overlay + auto-profile selection
    └── integration_linux.rs    # (target) Linux enforcement harness
```

**Embeddable crate.** All engine modules (`error`, `sandbox`, `profile`, `env`, `home`,
`filter`, `resolve`, `backends`) are `pub` and re-exported from `src/lib.rs`. The CLI
(clap + serde_yaml) is behind the default-on `cli` feature; embedders use
`default-features = false` to get the engine only.

**Two binaries.** `isol8` (main, always unprivileged; `required-features = ["cli"]`) and
`isol8-net-helper` (Phase 3, file-capability `cap_net_admin+ep`). The helper
sets up netns/veth/nftables, drops privilege, then execs into the prepared
namespace. The main binary never needs root.

---

## 2. Data flow (one `isol8 <cmd>` invocation)

```
cli::Cli::parse()  [feature = "cli"]
   │  Spec { profiles, profile_paths, auto_profiles, add_dirs_rw/ro, home, … cmd }
   ▼
cli::config::load()                      ── isol8.toml/yaml (cwd, ISOL8_CONFIG_PATH,
   │  Config { default_profiles, auto_profiles, profile_paths, … }   or ~/.config/isol8/)
   ▼
config::apply_to_spec() + apply_env_overrides()   ── precedence: defaults < config < ISOL8_* < CLI
   ▼
resolve::effective_policy(&Spec)          ← also called directly by Sandbox::run/spawn/dry_run
   │
   ├─ profile::LayerRegistry::load(profile_paths)
   │     builtin (build.rs embed) → user config dir → profile-path overlays
   │
   ├─ filter::RunContext::from_cmd(&cmd)
   ├─ profile::select_layer_names()        ── default_profiles + --profile + auto_profiles
   │     (executable filter match on layer.filter.executables)
   ├─ profile::resolve_requires()           ── transitive requires, cycle detect, dedup
   ├─ filter::apply_layer_filter() per layer   ── skip grants when os/arch/executable mismatch;
   │     fold matching [[policies]] into layer
   ├─ home::resolve(&spec, &layers)        ── R4: effective $HOME FIRST (default: real home; replacement opt-in);
   │                                          --no-seed clears the seed list
   ├─ profile::load_merged()               ── ~ + #HOME expansion, --add-dirs-* override layer, merge
   └─ env::build_minimal()                 ── R3.1 allowlist, HOME first, then --env-pass / --set-env
   │  EffectivePolicy { layer_names: Vec<(name, LayerOrigin)>, profile, env, home }
   ▼
backends::select()
   │
   ├── dry_run / --show-policies ?
   │     sandbox::dry_run(&Spec) → DryRun { layer_names, profile, env, cmd, policy, policy_label }
   │     cli: print_dry_run(&DryRun) ; return
   ▼
home::seed(&effective.home)              ── R4.4 read-only seed into scratch home (first-creation-only)
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

### `src/cli/` — feature `cli` (clap + serde_yaml)

No `run` subcommand — the confined command is passed directly. Meta/admin commands
use an `@` prefix (`cli::META_PREFIX`) so they never collide with the confined argv.

```rust
// src/cli/mod.rs  — pub fn main() -> anyhow::Result<()>

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
`src/cli/config.rs` — global config discovery and `ISOL8_*` env overrides.
`src/cli/diag.rs` — `@diag` delta-debug helper (macOS only).

### `profile.rs` — the core (drives everything)

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

### `config.rs`

```rust
pub struct Config {
    pub default_profiles: Vec<String>,  // e.g. ["base", "macos/system-runtime"]
    pub auto_profiles: bool,
    pub profile_paths: Vec<String>,
    pub add_dirs_rw: Vec<String>,
    pub add_dirs_ro: Vec<String>,
    pub home: Option<String>,
    pub dry_run: bool,
}
```

Discovery: `ISOL8_CONFIG_PATH` (file or dir) → `./isol8.toml|yaml` →
`~/.config/isol8/isol8.toml`. `ISOL8_PROFILE`, `ISOL8_PROFILE_PATH`,
`ISOL8_ADD_DIRS_RW`, `ISOL8_HOME`, `ISOL8_DRY_RUN`, etc. mirror CLI flags.

### `filter.rs`

`RunContext { cmd, os, arch }`, `filter_matches`, `apply_layer_filter`,
`apply_policies` (fold `[[policies]]` into unconditional fields when filter matches).

### `resolve.rs`

`effective_policy(&RunArgs) -> EffectivePolicy` — shared pipeline for `run`,
`policies show`, and `--dry-run`. `EffectivePolicy.layer_names` is the resolved
(deps-first) stack tagged with `LayerOrigin` (`Explicit` / `Auto` / `Required`) so
`--show-policies` shows *why* each layer contributes. `parse_set_env(&[String])`
parses `--set-env K=V` pairs (errors on a missing `=`, no silent drop) before
`env::build_minimal`. `confine_executable(&mut Profile, &mut [String])`
— called only on the exec paths (`run`, `@diag`): resolves `cmd[0]` execvp-style
against the host `PATH` to an absolute path (clean `command "x" not found` on miss)
and auto-grants the resolved binary `ro` so deny-by-default never hides the
command's own executable (e.g. an agent under `~/.local/bin`).

### `home.rs` — R4, first-class

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

### `env.rs` — R3

`build_minimal(&Profile, &Path, env_pass: &[String], set_env: &[(String,String)])
-> HashMap<String,String>`. Filters `std::env` to the allowlist
(`HOME, PATH, SHELL, TMPDIR, USER, LOGNAME, PWD`), applies the resolved HOME first,
folds profile env (no override), then applies CLI controls highest-precedence:
`--env-pass NAME` pulls a named host var through, `--set-env K=V` sets one
explicitly. The `ISOL8_SANDBOXED` marker is stamped last so `--set-env` can't clear
it. (`--env-file` is still future.)

### `backends/mod.rs`

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

### `spawn.rs`

`exec(cmd, env, policy_hook) -> Result<i32>`. Applies the backend's pre-exec hook
(no-new-privs, ruleset restrict, env_clear+envs), spawns, waits, returns exit code.
Ensures clean teardown when the process tree exits (R1.4) — namespaces/cgroups
collapse on last-process exit; `PR_SET_PDEATHSIG`-equivalent for orphan cleanup.

### `caps.rs`

Capability probing/dropping via `caps`/`nix`. Used by `backends::probe`, the net
tier selector, and the N3 helper (drop privilege before exec, R5.6).

### `src/bin/isol8-net-helper.rs`

Standalone privileged helper (Phase 3). Creates gateway netns + veth, installs
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
- **Trust via transparency.** `--dry-run` / `isol8 policies show` render the layer
  stack and exact effective policy; `isol8 profiles resolve` shows which layers matched.
- **Config precedence.** Built-in defaults < config file < `ISOL8_*` env < CLI flags.
- **Profile-path overlay.** External dirs/files override same-named built-in layers;
  missing profile-path entries are hard errors (unlike the optional user config dir).

---

## 5. Build targets per phase

| Phase | Modules that become real |
|---|---|
| 1 | `cli`, `profile.rs`, `config`, `filter`, `resolve`, `build.rs`, `env`, `home`, `backends/{linux,macos}` (MVP) |
| 2 | full `env` flags, R1.3 limits in `linux`, structured JSON policy dump, WSL2 paths |
| 3 | `net/*`, `caps`, `src/bin/isol8-net-helper.rs` |
| 4 | seccomp in `linux`, JSON export in `render`, `tests/integration_*` |
| 5 | `backends/windows` |
