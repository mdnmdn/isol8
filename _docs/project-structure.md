# isol8 — Target Project Structure & Code Blueprint

> The intended layout of the full `isol8` crate once all phases land, with
> module responsibilities, key types, and data flow. This document is the
> *destination*; the current tree (Phase 1, macOS MVP working — see
> [`AGENTS.md`](../AGENTS.md)) deliberately consolidates some of it:
>
> - `profile/` is a single `src/profile.rs` (types + load + merge + `resolve_requires`),
>   not a submodule dir, and there is no separate `profile/render.rs`.
> - `--dry-run` rendering (`render_dry_run`) lives in `src/backends/mod.rs`, and the
>   spawn/exec logic is inside each backend rather than a separate `src/spawn.rs`.
> - A `src/lib.rs` exposes the modules so the binaries and `tests/` share the crate.
> - `home.rs`, `env.rs`, `backends/macos.rs`, and the `isol8-field-test` bin are real;
>   `net/`, `caps.rs`, the N3 helper, and `backends/linux.rs` are still stubs/future.
>
> Companion to the requirements in [`project-description.md`](./project-description.md).
> Section refs (R1–R6, N0–N3, §7) point there.

---

## 1. Crate layout (target)

```
isol8/
├── Cargo.toml                  # workspace-less single crate; main bin + net helper bin
├── AGENTS.md
├── _docs/
│   ├── project-description.md  # requirements + ecosystem research
│   └── project-structure.md    # this file
├── profiles/                   # built-in TOML profile layers, embedded at build time
│   ├── base.toml
│   ├── rust.toml
│   ├── node.toml
│   ├── python.toml
│   └── net/
│       ├── github.toml
│       └── npm.toml
├── src/
│   ├── main.rs                 # bin entrypoint: parse → resolve → apply → exec
│   ├── cli.rs                  # clap definitions
│   ├── profile/
│   │   ├── mod.rs              # Profile, ProfileLayer, Access, PathGrant, merge
│   │   ├── load.rs             # embedded defaults + user TOML dir discovery
│   │   └── render.rs           # backend-agnostic effective-policy view (--dry-run)
│   ├── env.rs                  # sanitized environment construction (HOME first)
│   ├── home.rs                 # R4 effective-home resolution + seeding
│   ├── spawn.rs                # cross-platform child exec + teardown + exit code
│   ├── backends/
│   │   ├── mod.rs              # Backend trait, select(), capability probe
│   │   ├── linux.rs           # Landlock ruleset + optional user/mount ns
│   │   ├── macos.rs           # Seatbelt policy text + sandbox-exec
│   │   └── windows.rs         # AppContainer + Job Objects (Phase 5, stub until then)
│   ├── net/
│   │   ├── mod.rs             # NetTier, NetworkPolicy, tier auto-select (R5.7)
│   │   ├── proxy.rs          # N1 filtering proxy (hostname/SNI; optional MITM)
│   │   ├── pasta.rs          # N2 rootless userspace stack orchestration
│   │   └── helper.rs         # N3 client side: talk to isol8-net-helper
│   └── caps.rs                # capability probing/dropping (CAP_NET_ADMIN, userns)
├── src/bin/
│   ├── isol8-net-helper.rs # small privileged N3 helper (setcap cap_net_admin+ep)
│   └── isol8-field-test.rs # real-sandbox field tests (see _docs/testing-strategies.md)
└── tests/
    ├── profile_merge.rs        # unit-ish: deny-first merge semantics
    └── integration_linux.rs    # run real cmds, assert allowed/denied (Linux only)
```

**Two binaries.** `isol8` (main, always unprivileged) and
`isol8-net-helper` (Phase 3, file-capability `cap_net_admin+ep`). The helper
sets up netns/veth/nftables, drops privilege, then execs into the prepared
namespace. The main binary never needs root.

---

## 2. Data flow (one `isol8 run` invocation)

```
cli::Cli::parse()
   │  RunArgs { profiles, add_dirs_rw/ro, home, env flags, net tier, dry_run, cmd }
   ▼
home::resolve(&run)                      ── R4: effective $HOME FIRST
   │  EffectiveHome { path, seed: Vec<SeedEntry> }
   ▼
profile::load(&run.profiles)             ── embedded defaults + user TOML dir
   │  Vec<ProfileLayer>
   ▼
profile::resolve_requires(selected)      ── inheritance: transitive `requires`,
   │  Vec<ProfileLayer>  (deps-first)        cycle detect, dedup, topo-sort (band tiebreak)
   ▼
profile::merge(layers, &overrides)       ── deny-first union; folds --add-dirs-*,
   │  Profile { paths, env, home_replace, network }   home, env, net into final layer
   ▼
env::build_minimal(&profile, &home)      ── R3.1 allowlist, HOME applied first
   │  HashMap<String,String>
   ▼
caps::probe()  +  backends::select()     ── strongest supported net tier (R5.7)
   │
   ├── run.dry_run ? profile::render::dump(&profile, &env, &cmd) ; return
   ▼
backend.spawn(&profile, &env, &cmd)      ── apply OS policy, exec, wait
   │  i32 exit code
   ▼
std::process::exit(code)
```

**Ordering invariant:** `home::resolve` runs *before* `profile::merge`, so every
`$HOME`-relative grant in every layer is computed against the replacement home, not
the real one (R4.2/R4.6).

---

## 3. Module blueprints

### `cli.rs`

clap derive. `Cli { command: Command }`, `Command::Run(RunArgs)`. `RunArgs` carries
every knob from the spec:

```rust
pub struct RunArgs {
    pub profiles: Vec<String>,        // --profile (repeatable, R6 layers)
    pub add_dirs_rw: Vec<String>,     // --add-dirs-rw (R2.5)
    pub add_dirs_ro: Vec<String>,     // --add-dirs-ro (R2.5)
    pub home: Option<String>,         // --home (R4.1)
    pub env_pass: Vec<String>,        // --env-pass NAMES (R3.2)
    pub env_file: Option<String>,     // --env=FILE   (R3.3)
    pub env_inherit: bool,            // --env full passthrough escape hatch (R3.4)
    pub net_tier: Option<NetTier>,    // --net n0|n1|n2|n3 (R5); default = auto
    pub enable: Vec<String>,          // --enable github,npm (R5.3 / R6 opt-in layers)
    pub dry_run: bool,                // --dry-run effective policy dump
    pub cmd: Vec<String>,             // trailing_var_arg, the confined command
}
```

### `profile/mod.rs` — the core (drives everything)

```rust
pub enum Access { None, Ro, Rw }            // default deny = None

pub struct PathGrant { pub path: String, pub access: Access }

pub struct HomeReplace { pub enabled: bool, pub auto_scratch: bool, pub seed: Vec<String> }

pub struct NetworkPolicy { pub tier: NetTier, pub allow_domains: Vec<String> }

// One TOML layer as authored.
pub struct ProfileLayer {
    pub name: String,
    pub requires: Vec<String>,            // inheritance edges (alias `extends`)
    pub paths: Vec<PathGrant>,
    pub env: HashMap<String, String>,
    pub home_replace: Option<HomeReplace>,
    pub network: Option<NetworkPolicy>,
    pub macos: Option<MacosExtra>,        // macOS-only caps + raw SBPL passthrough
}

/// Expand selected layers over their transitive `requires` graph.
/// Topo-sort, deps-first; cycle detection (error), dedup, band-number tiebreak.
/// Runs before merge — see _docs/profile-model.md §3.
pub fn resolve_requires(selected: &[String]) -> Result<Vec<ProfileLayer>>;

// The merged, effective policy handed to a backend.
pub struct Profile {
    pub paths: Vec<PathGrant>,
    pub env: HashMap<String, String>,
    pub home_replace: Option<HomeReplace>,
    pub network: NetworkPolicy,
}

/// Deny-first union (R2.4/R6). Order = base → system → network → toolchains →
/// shared → integrations → --enable → auto-detected → workdir → custom → appended.
/// Per path: most-recent explicit grant wins; env merged without override unless
/// --env escape; network allowlist = union of enabled layers.
pub fn merge(layers: &[ProfileLayer], overrides: &Overrides) -> Profile;
```

TOML schema (authoring side), matching spec §7:

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
paths = [ { path = "~/.cargo", access = "rw" } ]
```

- `profile/load.rs` — `load(names) -> Vec<ProfileLayer>`. Built-in layers embedded
  via `include_str!` from `profiles/`; user layers read from a config dir
  (`$XDG_CONFIG_HOME/isol8` or platform equivalent). Auto-detection heuristics
  (`--profile-for cargo`) live here.
- `profile/render.rs` — `dump(&Profile, &env, &cmd)` for `--dry-run`: human-readable
  effective grants + env + resolved tier. Machine-readable (JSON) export for agent
  frameworks is a Phase 4 add here.

### `home.rs` — R4, first-class

```rust
pub struct EffectiveHome { pub path: PathBuf, pub seed: Vec<SeedEntry> }

/// CLI --home > profile home_replace > auto scratch (tempfile under
/// /tmp or XDG_RUNTIME_DIR). Resolved before profile merge.
pub fn resolve(run: &RunArgs, layers: &[ProfileLayer]) -> Result<EffectiveHome>;

/// Copy/bind allowlisted real-home entries read-only into the scratch home (R4.4).
pub fn seed(home: &EffectiveHome) -> Result<()>;
```

### `env.rs` — R3

`build_minimal(&Profile, &EffectiveHome) -> HashMap<String,String>`. Filters
`std::env` to the allowlist (`HOME, PATH, SHELL, TMPDIR, USER, LOGNAME, PWD`),
applies the resolved HOME first, then folds profile env (no override), then
`--env-pass`/`--env-file`/`--env` per the flags. On WSL2, strips `WSLENV` and
Windows `PATH` segments.

### `backends/mod.rs`

```rust
pub trait Backend {
    fn spawn(&self, profile: &Profile, env: &HashMap<String,String>, cmd: &[String]) -> Result<i32>;
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
  takes `EffectiveHome` so no layer can compute a grant against the real home.
- **Deny-by-default.** `Access::None` is the implicit default; backends start from a
  closed policy and only open what the merged `Profile` lists.
- **Unprivileged main.** Only `isol8-net-helper` holds a file capability; the
  main binary never escalates.
- **Single binary, no daemons.** No persistent state; scratch homes are temp dirs
  cleaned on exit.
- **Trust via transparency.** `--dry-run` renders the exact effective policy;
  backends surface *why* an access was denied with actionable fixes.

---

## 5. Build targets per phase

| Phase | Modules that become real |
|---|---|
| 1 | `cli`, `profile/*`, `env`, `home`, `spawn`, `backends/{linux,macos}` (MVP) |
| 2 | full `env` flags, R1.3 limits in `linux`, `profile/render` dump, WSL2 paths |
| 3 | `net/*`, `caps`, `src/bin/isol8-net-helper.rs` |
| 4 | seccomp in `linux`, JSON export in `render`, `tests/integration_*` |
| 5 | `backends/windows` |
