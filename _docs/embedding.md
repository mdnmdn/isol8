# isol8 — Embedding guide

How to drive isol8 from another program. Three routes, in order of preference:

| Route | For | Cost |
|-------|-----|------|
| **Rust library** (`isol8` facade) | Rust hosts | one crate dependency |
| **Subprocess + `--json`** | Python, Node, C#, Go, shell | none — spawn the binary |
| C ABI (`isol8-ffi`) | in-process non-Rust hosts | **not built** — see [§7](#7-windows-and-in-process-hosts) |

Embedding isol8 inside a **host that runs other programs** (agent manager, task
harness, CI runner)? Read [integration.md](./integration.md) — it covers the
architecture this reference does not: extension points, host-owned config,
generated per-session profiles and homes, registry admission.

Companion docs: [config.md](./config.md) (the config contract),
[profile-model.md](./profile-model.md) (what a policy *is*),
[instructions.md](./instructions.md) (the CLI),
[project-structure.md](./project-structure.md) (crate layout).

---

## 1. Feature matrix

Depend on the root **facade** package `isol8` so `use isol8::…` stays stable across
workspace changes.

```toml
# Engine only — no clap, no dialoguer, no registry I/O.
isol8 = { version = "0.2", default-features = false }

# Engine + offline registries (recipe sources, lockfile, trust).
isol8 = { version = "0.2", default-features = false, features = ["registry"] }

# + cage authoring API (render / apply / drift / bundles), still no clap.
isol8 = { version = "0.2", default-features = false, features = ["wizard"] }

# Everything, including the clap CLI and interactive prompts.
isol8 = { version = "0.2" }
```

| Feature | Adds | Pulls in |
|---------|------|----------|
| *(none)* | `isol8-core`: profiles, config, resolve, home plan, recipes, detect, analyze, backends | serde, toml, serde_yaml, thiserror |
| `registry` | `isol8::registry::*`, `ensure_registry_provider()` | + `isol8-registry` |
| `wizard` | `isol8::wizard::*` — cage authoring | + `isol8-cli` (no clap) |
| `cli` *(default)* | `isol8::cli::*`, the binary, interactive wizard | + clap, dialoguer |

`default = ["cli", "registry"]`.

**One call to remember.** If you use `[registries.*]`, call
`isol8::ensure_registry_provider()` once at startup so offline recipe directories
are discovered. It is process-global and first-call-wins. Config discovery
(`config_path`, `ISOL8_CONFIG_PATH`, `@…`, `@managed/<id>`) needs **no**
registration — `isol8::config` owns it.

---

## 2. The pipeline

Everything funnels through one path. Each stage is a public function, so you can
stop anywhere and inspect.

```
Config          config::load()                  isol8.toml + markers + ISOL8_*
   │
   ▼
Spec            resolve::spec_from_config()     what to confine + how
   │            (or Sandbox builder / Spec::new)
   ▼
EffectivePolicy resolve::effective_policy_in()  layers merged, HOME resolved, env built
   │
   ▼
HomePlan        effective.home.plan             mutations, not yet applied
   │            home::materialize()             ← the only step that touches disk
   ▼
spawn           backends::select().spawn()      OS policy applied, process running
   │            SandboxChild { id, wait, kill }
   │            …or spawn_with_stdio() / spawn_pty() for a confined pty (unix)
```

**Invariant:** the effective `$HOME` is resolved *before* any path grant is
computed. Nothing you can call reorders that.

---

## 3. Tasks

### Run a command

```rust
let code: i32 = isol8::Sandbox::new()
    .profile("base")
    .grant_rw("/my/project")
    .run(["node", "script.js"])?;
```

Non-blocking:

```rust
let mut child = isol8::Sandbox::new().profile("base").spawn(["sleep", "5"])?;
let pid = child.id();
let code = child.wait()?;      // or child.kill()?
```

Builder methods: `profile`, `profile_path`, `auto_profiles`, `grant_rw`,
`grant_ro`, `cwd_ro`, `home`, `ephemeral_home`, `home_op`, `toolchain`,
`recipe_path`, `no_seed`, `env_pass`, `set_env`, plus `from_spec` / `spec_mut`
to drop down to the raw [`Spec`]. Terminals: `run`, `spawn`, `dry_run`,
`spawn_with_stdio`, `spawn_pty`.

### Run a command on a pseudo-terminal (unix)

An interactive agent harness drives a full screen — alternate screen, absolute
cursor addressing, raw keystrokes, resize redraw — so it needs a **controlling
terminal** and its geometry has to reach the kernel, or it redraws at the wrong
size. `spawn_pty` opens the pty for you:

```rust
use isol8::{PtySize, Sandbox};

let mut pty = Sandbox::new()
    .profile("base")
    .grant_rw("/my/project")
    .spawn_pty(["claude"], PtySize { cols: 120, rows: 40 })?;

let reader = pty.try_clone_reader()?;      // File — feed the host's pump
let writer = pty.take_writer()?;           // File — keystrokes in
pty.resize(PtySize { cols: 132, rows: 50 })?;   // on the host's SIGWINCH
let code = pty.child().wait()?;            // or pty.child().kill()?
```

`PtyChild` also has `master()` (a `BorrowedFd`), `get_size()` and `into_parts()`.
Dropping it closes the master (SIGHUP to the session) but neither waits for nor
kills the child — own that lifecycle explicitly.

**There is no supervisor shim.** macOS `sandbox-exec` `execve`s in place and Linux
forks exactly once, so `pty.child().id()` is the harness's own pid: `kill` and
`wait` behave exactly as for an unconfined pane, with no signal forwarding and no
orphaning when a tab closes.

If the host already owns a pty (`portable-pty`, or its own `openpty`), hand over
the slave instead:

```rust
use isol8::{SandboxStdio, Sandbox};

let stdio = SandboxStdio::from_tty(slave)?;    // dup'd 3×, ctty requested
let mut child = Sandbox::new().profile("base").spawn_with_stdio(["claude"], stdio)?;
```

`SandboxStdio::from_fds(stdin, stdout, stderr)` is the non-tty form (pipes, files)
— same wiring, no controlling terminal. `isol8::open_pty(size)` is public if you
want the pair without the `PtyChild` wrapper, and `PtyChild::from_parts` re-attaches
one afterwards.

Two narrow behaviours apply **only** when `controlling_terminal` is set; the seam
does not otherwise widen the policy:

- `TERM` and `COLORTERM` are passed through from the host environment as
  *defaults*. The env allowlist ([§R3](project-description.md)) drops them, and a
  TUI harness with no `TERM` cannot decide what it may draw — it starts blank or
  refuses to run, which reads as a crash rather than as a denial. Profile env,
  `env_pass` and `set_env` still override.
- On macOS the `pseudo-tty` capability is added to the rendered Seatbelt policy,
  since a policy without `(allow pseudo-tty)` fails pty operations the same
  confusing way.

**Nesting is per-process.** `sandbox::ensure_not_nested()` fails when
`ISOL8_SANDBOXED` is set, so a host that is itself confined can never confine a
session. Probe it **once at startup** and report it as a capability — a per-pane
`Error::NestedSandbox` is exactly the confusing failure a pty host should avoid.

Windows (ConPTY) is not supported: the whole seam is `cfg(unix)`.

### Build a Spec directly

`Spec` is `#[non_exhaustive]` — use `Spec::new` plus field assignment, never a
struct literal, so new fields are not a breaking change:

```rust
let mut spec = isol8::Spec::new(["claude"]);
spec.profiles = vec!["base".into(), "agents/claude-code".into()];
spec.add_dirs_rw = vec!["/my/project".into()];
spec.home = Some("@managed/work".into());
```

### Load config exactly as the CLI does

```rust
let mut cfg = isol8::config::load()?;          // env → project marker → OS default
isol8::config::apply_env_overrides(&mut cfg);  // ISOL8_PROFILE, ISOL8_HOME, …

let ctx = isol8::Context::from_environment()?;
let spec = isol8::resolve::spec_from_config(
    &cfg,
    isol8::Spec::default(),   // pre-set fields here win over the config
    vec!["claude".into()],
    &ctx,
)?;
```

`spec_from_config` applies the full precedence chain from
[config.md §7](./config.md): builtin defaults → config file (+ marker overlay) →
`ISOL8_*` → fields you pre-set → cage (fills what is still empty).

### Resolve a cage

```rust
let ctx = isol8::Context::from_environment()?;
let cfg = isol8::config::load()?;

let name = isol8::cage::select_name(Some("work"), &cfg);  // flag → ISOL8_CAGE → cfg.cage
if let Some(cage) = isol8::cage::resolve_in(name.as_deref(), &ctx.cwd, Some(&ctx.config_dir))? {
    let mut spec = isol8::Spec::new(["echo", "hi"]);
    isol8::cage::apply_overlay(&cage.overlay(), &mut spec);
}
```

Also: `cage::list_cages_in`, `cage::load_from_path`, `cage::write_new`,
`cage::format_show`.

### Enumerate and compile recipes

```rust
isol8::ensure_registry_provider();                       // if you use [registries.*]

let reg = isol8::RecipeRegistry::load(&[])?;             // builtins + user + registries
for id in reg.ids() { println!("{id}"); }

let rc = isol8::filter::RunContext::from_cmd(&[]);
let recipe = reg.resolve("toolchains/nvm", &rc)?;        // platform-filtered variant
let choice = isol8::ToolchainChoice::new("nvm", "link")?;
let contribution = reg.compile(&choice, &rc)?;           // → grants + home ops + env
```

Recipes are cached in the `RecipeRegistry` value — build it once and reuse it;
`load` walks the builtin table, the user config dir, and any offline registry
directories.

### Detect toolchains / verify a cage

```rust
let real = isol8::home::real_home();
let rows = isol8::detect::detect_all(&reg, &rc, &real)?;     // read-only probes
print!("{}", isol8::detect::format_detect_table(&rows));

let results = isol8::detect::verify_toolchains(&spec)?;      // runs smoke tests confined
```

`DetectResult` and `VerifyResult` are structured (`found`, `ok`, `detail`,
`fix_hint`) — act on the fields, don't parse the table.

### Create a managed home

```rust
let ctx = isol8::Context::from_environment()?;
let path = ctx.managed_home("work")?;                // {config_dir}/homes/work

let ops = vec![
    isol8::HomeOpSpec::mkdir("~/.cache"),
    isol8::HomeOpSpec::link("#HOME/.nvm", "~/.nvm"),  // #HOME = real home
];
let plan = isol8::HomePlan::compute(&ops, &ctx, &path)?;
println!("{}", plan.render());                       // preview — nothing written yet
plan.apply()?;                                       // idempotent
```

Tokens: `~` = effective home, `#HOME` = real home, `@managed/<id>` = managed root,
`@…` = config dir.

### Dry-run a policy

```rust
let dry = isol8::Sandbox::new().profile("base").dry_run(["node", "x"])?;
for (name, origin) in &dry.layer_names { println!("{name} ({})", origin.label()); }
println!("{}", dry.policy);            // SBPL / Landlock rules
println!("{}", dry.home_plan.render());
```

`dry_run` never mutates the filesystem.

### Run and analyze denials

```rust
let ctx = isol8::Context::from_environment()?;
let outcome = isol8::analyze::run_and_analyze(&spec, &ctx)?;
println!("{}", outcome.report.render());
for item in &outcome.report.items {
    if let Some(id) = &item.recipe_id { println!("suggest: {id}"); }
}
```

Denials come from the macOS unified log, or from an NDJSON feed
(`ISOL8_ANALYZE_FEED`) on any platform — useful for tests and for Linux, where
Landlock emits no denial log. Observation is best-effort and non-exhaustive; the
report says so.

### Author a cage (feature `wizard`)

```rust
let req = isol8::wizard::WizardRequest { /* name, home, tools, dirs, … */ };
let preview = isol8::wizard::render(&req)?;          // the TOML that would be written
for note in isol8::wizard::preview_security_notes(&req.tools, &reg) { eprintln!("{note}"); }

let state = isol8::wizard::state_path();
match isol8::wizard::check_drift(&req.name, &path, &isol8::wizard::load_state(&state)?)? {
    isol8::wizard::DriftStatus::Clean => { isol8::wizard::apply(&req, &state)?; }
    drift => eprintln!("hand-edited: {drift:?}"),
}
```

The interactive prompting stays in the CLI; these are the same steps it drives.

---

## 4. Hermetic operation (`_in` variants)

The default entry points read `HOME`, the cwd, and `ISOL8_*` from the process
environment. When that environment belongs to a **host** rather than to isol8,
use the explicit-[`Context`] variants. Real home, cwd, config dir, `@…` /
`@managed/<id>` expansion, cage discovery and the automatic working-directory
grant then all come from the `Context` you pass — see the one remaining ambient
read below the table:

| Ambient | Hermetic |
|---------|----------|
| `config::load()` | `config::load_in(&ctx)` |
| `resolve::effective_policy(&spec)` | `resolve::effective_policy_in(&spec, &ctx)` |
| `sandbox::dry_run(&spec)` | `sandbox::dry_run_in(&spec, &ctx)` |
| `sandbox::spawn_with_stdio(&spec, stdio)` | `sandbox::spawn_with_stdio_in(&spec, &ctx, stdio)` |
| `sandbox::spawn_pty(&spec, size)` | `sandbox::spawn_pty_in(&spec, &ctx, size)` |

```rust
let ctx = isol8::Context {
    real_home: "/home/agent".into(),
    cwd: "/srv/work".into(),
    platform: isol8::Platform::Linux,
    config_dir: "/etc/isol8".into(),
    managed_root: "/var/lib/isol8/homes".into(),
};
let policy = isol8::resolve::effective_policy_in(&spec, &ctx)?;
```

This is also how you resolve a Windows cage on Linux for CI linting.

**One ambient read remains.** Layer and recipe *overlays from the user config
directory* (`$XDG_CONFIG_HOME/isol8/{profiles,recipes}`, else
`$HOME/.config/isol8/…`) are discovered from the process environment even on the
`_in` path — `Context::config_dir` does not redirect them. Built-in layers and
anything you pass in `Spec::profile_paths` / `Spec::recipe_paths` are unaffected.
A host that must be fully independent of the invoking user's dotfiles should
carry its own layers in `profile_paths` and not rely on that directory being
absent.

---

## 5. Machine-readable output

Every report type derives `Serialize`: `DryRun`, `EffectivePolicy`, `HomePlan`,
`PlannedOp`, `DetectResult`, `VerifyResult`, `AnalysisReport`, `Cage`, `Recipe`,
`Profile`.

```rust
let json = serde_json::to_string_pretty(&dry)?;
```

From another language, spawn the binary with `--json`:

```sh
isol8 --show-policies --json echo hi | jq '.profile.paths'
isol8 @cage detect --json          | jq '.[] | select(.found)'
isol8 @cage verify work --json     | jq '.[] | select(.ok == false)'
isol8 --analyze --json -- claude   | jq '.report.items'
isol8 @registry list --json
isol8 @profiles-list --json
```

Exit codes are the confined process's own, so a wrapper can pass them through.

---

## 6. Errors

```rust
pub enum Error {
    CommandNotFound(String), InvalidEnv(String), NestedSandbox,
    UnsupportedOs(&'static str), PolicyRejected(String), Profile(String),
    Io(std::io::Error), Toml(toml::de::Error), Message(String),
}
```

`isol8::Result<T> = Result<T, Error>`. The enum is `#[non_exhaustive]` — always
include a `_ =>` arm. `Message` is the contextual catch-all; match on the named
variants for programmatic decisions (`NestedSandbox` means you are already inside
an isol8 sandbox — Seatbelt cannot nest).

### Forward compatibility

Types the **engine produces** are `#[non_exhaustive]`, so a new field is not a
breaking change: `Spec`, `DryRun`, `EffectivePolicy`, `AnalysisReport`,
`AnalyzeOutcome`, `AnalyzeOptions`, `DetectResult`, `VerifyResult`, `Cage`,
`Error`. Read their fields; never match or destructure exhaustively, and build a
`Spec` with `Spec::new` (or `..Default::default()`) rather than a struct literal.

Types you **hand to the engine** stay constructible by struct literal, because
that is their purpose: `Context` (§4), `CageOverlay`, `HomeOpSpec`,
`ToolchainChoice`, `Config`. Adding a field to one of these *is* a semver break
and is treated as such.

---

## 7. Windows and in-process hosts

**Path grants are not enforced on Windows today.** The AppContainer backend
isolates the *process* (see [windows-support.md](./windows-support.md)); the
per-path ro/rw model that Seatbelt and Landlock provide is **documentary only**.
Do not present a Windows run as path-confined. `--show-policies` and the `--json`
`DryRun` label those grants explicitly.

Enforcement will require an injected user-mode hook DLL (`isol8-winhook`), which
does not exist yet. Requirements that shape the API you depend on today:

- **The DLL is a build artifact, not a crate item.** `cargo add isol8` will not
  produce it. Resolution order will be `Spec.win_hook_dll` → `ISOL8_WINHOOK_DLL`
  → next to `current_exe()` → `%LOCALAPPDATA%\isol8\bin`. `Spec` is
  `#[non_exhaustive]` so adding that field is not a break.
- **No silent downgrade.** If the hook is required and cannot load, the run fails
  with a typed error rather than running unconfined.
- **Arch-matched.** An x64 host spawning an x86 or ARM64EC child needs the
  matching DLL; selection happens inside the backend after `CREATE_SUSPENDED`.
  No public API change — which holds only because `SandboxChild`'s constructors
  are `pub(crate)`.

**If your host is itself a DLL** (a .NET / Electron / C++ addin):

- Library paths never call `std::process::exit` — that is confined to the CLI.
- Use the `_in` variants from [§4](#4-hermetic-operation-_in-variants); the
  process environment belongs to the host, not to isol8.
- `ensure_registry_provider()` is process-global and first-call-wins. Two hosts
  in one process cannot install different providers.

**Non-Rust in-process hosts:** there is no C ABI. Use the subprocess + `--json`
route in [§5](#5-machine-readable-output); it covers everything the library does
except long-lived `SandboxChild` handles, which the binary owns anyway.

---

## 8. Not extension points

- **`Backend` cannot be implemented outside the crate.** The trait is public and
  object-safe, but `Backend::spawn` must return a `SandboxChild` whose
  constructors are `pub(crate)`. Third-party backends are deliberately not
  supported — a sandbox with a pluggable enforcement layer is not a sandbox.
  Because the trait is closed, *adding* a method to it (as `spawn_with_stdio`
  did) is not a breaking change for embedders.
- **Profiles are data, not code.** Extend policy through TOML layers
  (`--profile-path` / `Spec::profile_paths`), not by patching the merge.
- **`@registry` orchestration and `@diag`'s minimizer stay in the CLI.** Their
  primitives (`open_offline`, `update_registry`, `diff_index`,
  `verify_lock_against_disk`) are public in `isol8::registry`; the command-level
  glue is not part of the library contract.

---

## 9. Examples

Runnable, and built by `just ci` so they cannot rot:

| Example | Features | Shows |
|---------|----------|-------|
| `embed_minimal` | none | the smallest working confinement |
| `embed_config` | `registry` | config → Spec → dry-run, headless CLI parity |
| `embed_cage` | `registry` | cage discovery → overlay → run |
| `embed_recipes` | `registry` | enumerate recipes, detect, materialize a home |
| `embed_analyze` | `registry` | run + denial analysis with an NDJSON feed |
| `embed_harness` | `registry` | host integration: per-session context, generated layer, managed home ([integration.md](./integration.md)) |
| `embed_wizard` | `wizard` | author a cage without clap |
| `embed_json` | none | serialize `DryRun` for a non-Rust consumer |

```sh
cargo run --example embed_config
cargo run --no-default-features --example embed_minimal
```
