# isol8 — Crate-as-library plan (Embedding API)

**Status:** complete — Steps 1–8 + docs landed and audited (2026-09-02).
Remaining work is §3 (Windows hook) and §6 (out of scope), both deliberately deferred.
**Depends on:** evolution Phase 9 (crate split) — done
**Companion docs:** [`multi-evo-plan.md`](./multi-evo-plan.md),
[`../config.md`](../config.md), [`../project-structure.md`](../project-structure.md),
[`../instructions.md`](../instructions.md), [`../../AGENTS.md`](../../AGENTS.md)
**Target:** post-0.2.6 — additive to the binary; no behaviour change for CLI users

---

## Context

Phase 9 split the codebase into `isol8-core` / `isol8-registry` / `isol8-cli` + an
`isol8` facade, with the goal *"clean API seams without behavior change"* and the
locked rule *"the CLI contains no policy logic"*.
[`../instructions.md`](../instructions.md) §Embedding and `README.md` advertise
`default-features = false` as a supported engine-only embed.

**The crate split landed; the API contract did not.** The engine primitives are
public and well-shaped — but the layer that turns them into isol8's *documented
behaviour* stayed private inside `isol8-cli`. A program embedding isol8 today cannot
reproduce what `isol8 -c work claude` does without reimplementing the config and
precedence rules from [`../config.md`](../config.md) by hand.

This plan closes that gap for three audiences:

1. **Rust embedders** — library API parity with the CLI.
2. **Non-Rust programs** — JSON over stdout (`--json`).
3. **Future Windows hosts** — injected-DLL enforcement without an API break.

### Progress board

| Step | Deliverable | Status | Gate |
|-----:|-------------|--------|------|
| 1 | `isol8-core::config` — one implementation | **done** (2026-07-28) | `just ci` |
| 2 | Precedence + cage merge on `Spec` | **done** (2026-07-28) | `just ci` |
| 3 | `Context` injection through the pipeline | **done** (2026-07-28) | `just ci` + field |
| 4 | `analyze::run_and_analyze` entry point | **done** (2026-07-28) | `just ci` |
| 5 | serde derives + `--json` | **done** (2026-07-28) | `just ci` |
| 6 | Wizard reachable without clap | **done** (2026-07-28) | `just ci` |
| 7 | `#[non_exhaustive]` + `Spec::new` | **done** (2026-09-02) | `just ci` |
| 8 | CI gates + de-CLI'd engine tests | **done** (2026-07-28) | `just ci` |
| — | Docs + examples | **done** (2026-07-28) | `cargo doc`, `--examples` |

### Landed so far

- **Behaviour fix (G3):** `ISOL8_*` no longer overrides explicit CLI flags. Env now
  applies to the loaded `Config`; flags win, as
  [`../config.md`](../config.md) §7 always claimed. Verified:
  `ISOL8_PROFILE=base isol8 --profile toolchains/rust --show-profiles` used to drop
  `toolchains/rust` entirely and now keeps it.
- **Config dedup (G1, G2):** `isol8-core/src/config.rs` is the only implementation;
  `isol8-registry` re-exports it and its ~130 duplicated lines are gone, as is the
  `context::set_config_dir_provider` `OnceLock`.
- **Precedence (G3, G4):** `resolve::spec_from_config`, `cage::select_name`,
  `cage::apply_overlay`. `prepare_run` / `prepare_opts` collapsed into one function
  that only bridges clap to the engine.
- **Context injection (G7):** `effective_policy_in`, `dry_run_in`, `config::load_in`.
- **Forward-compat (G8):** `Spec` is `#[non_exhaustive]` with `Spec::new`.
  Extended 2026-09-02 to every **engine-produced** type — `DryRun`,
  `EffectivePolicy`, `AnalysisReport`, `AnalyzeOutcome`, `AnalyzeOptions`,
  `DetectResult`, `VerifyResult`, `Cage`, `Error`. `Context` and `CageOverlay`
  deliberately stay **exhaustive**: embedders construct them by struct literal
  (that is the whole point of the hermetic `_in` variants and of
  `cage::apply_overlay`), so sealing them would remove the API rather than
  protect it. Documented in [`../embedding.md`](../embedding.md) §6.
- **Features (G1):** `wizard` feature reaches `isol8_cli::wizard` with **zero** clap
  or dialoguer in the tree; `cli/config.rs` and `cli/diag.rs` are `pub mod`.
- **CI (G9, G10):** `just ci` now compiles `--no-default-features` (engine-only) and
  `--features registry`; GitHub CI gained `--workspace`, so core's unit tests run
  there for the first time. `tests/profile_filters.rs` no longer imports
  `isol8::cli`.
- **Analyze (G5):** `analyze::run_and_analyze` / `run_and_analyze_with` +
  `AnalyzeOutcome` / `AnalyzeOptions` / `author_trace`. `analyze_cmd` shrank to
  flag handling and rendering.
- **Machine-readable (G6):** `Serialize` on every report type; `--json` on
  `--show-policies`, `--show-profiles`, `--analyze`, `@cage list|show|detect|verify`,
  `@registry list`, `@profiles-list`.
- **Docs (G11):** new [`../embedding.md`](../embedding.md) + seven
  `examples/embed_*.rs` built by `just ci`; corrected the stale `home::resolve` /
  `EffectiveHome` / `Backend` signatures in
  [`../project-structure.md`](../project-structure.md) and the pre-crate-split
  paths in [`../windows-support.md`](../windows-support.md).

### Wire-format contract

`--json` and the `Serialize` impls pin these enum spellings; changing them breaks
non-Rust consumers:

| Type | JSON |
|------|------|
| `LayerOrigin` | `explicit` \| `auto` \| `required` |
| `StrategyName` | `share` \| `link` \| `isolate` |
| `HomeOpKind` | `link` \| `mkdir` \| `seed-ro` \| `copy` |
| `PlanAction` | `apply` \| `skip-exists` \| `skip-missing` |
| `Platform` | `macos` \| `linux` \| `windows` \| `other` |
| `HomeMode` | `inherit` \| `ephemeral` \| the raw path string |
| `Access` / `DenialAccess` | lowercase (`ro`, `rw`, `read`, `write`, …) |

Parity check against pre-change output: `--show-policies` (plain, `claude`, and
under `ISOL8_CONFIG_PATH=./_data/config`), `@cage list`, `@registry list`,
`@profiles-list` are byte-identical. `@cage detect` differs only in flutter's
version string, which is a flaky external probe.

---

## 1. Audit — what is sound, what is not

### 1.1 Sound

| Capability | Entry point | Verdict |
|---|---|---|
| Build `Spec`, run / spawn / dry-run | `sandbox.rs:25,301,371-509` — 15 public fields, `Default`, builder | Complete |
| Merge layers, effective policy | `resolve::effective_policy` (`resolve.rs:51`) | Complete |
| Home plan / apply / materialize | `plan.rs:131,140`, `home.rs:223` | Complete |
| Cage load / resolve / list / overlay / author | `cage.rs:181,322,392,108,475,505` | Complete |
| Recipe load / enumerate / compile | `recipe.rs:542,673,680,740` (`RecipeRegistry::ids()`) | Complete |
| Detect / verify | `detect.rs:137,152,182,219,400` — structured results | Complete |
| Registry sources / lockfile / trust / diff | `isol8-registry` (`lib.rs:928,988,1188,1343,1542`) | Complete |
| Analyze from an NDJSON feed | `analyze.rs:212,246,427` | Complete |
| Typed errors, shared `Result` | `error.rs:14-65`; registry uses core's `Result` | Complete |

### 1.2 Not sound

**G1 — Config loading is unreachable at every feature level.**
`crates/isol8-cli/src/cli/mod.rs:512` declares `mod config;` — **private**. A private
module's items cannot be re-exported, so `pub use isol8_cli::cli::*`
(`src/lib.rs:63`) does not expose them. `Config`, `load()`, `apply_to_run`,
`apply_env_overrides`, `init_template`, YAML config support, and **every `ISOL8_*`
run-option override** are unreachable even with `features = ["cli"]`. Same for
`mod diag;` (`:513`).

**G2 — Config discovery is implemented twice and self-documents the drift risk.**
`isol8-registry/src/lib.rs:439-620` reimplements `PROJECT_CONFIG_MARKERS`,
`CONFIG_BASENAMES`, `resolve_config_location`, `find_config_in_dir`,
`discover_local_marker`, `expand_at_path`, `config_isol8_dir`,
`effective_config_dir`. The registry copy carries the comment *"Kept in sync with
`isol8-cli` `cli::config::PROJECT_CONFIG_MARKERS`"*. It is a **subset**:
`peek_local_meta` (`:534`) reads only `config_path` / `ignore_global`, while
`cli/config.rs:311-343` does a full per-field overlay merge. Two sources of truth
for one documented rule set ([`../config.md`](../config.md) §1–2).

**G3 — The precedence chain is not a library concept.**
[`../config.md`](../config.md) §7 documents
`builtin → config → marker → ISOL8_* → flags → cage`. It exists only as a private
call sequence in `prepare_run` (`cli/mod.rs:851-877`) and `prepare_opts`
(`:879-904`), operating on `ProfileOpts` — a clap `#[derive(Parser)]` struct
(`cli/mod.rs:14-21`). Policy sequencing on a clap type violates the Phase 9 rule.

**G4 — Cage name-resolution precedence is CLI-private.**
`cage::resolve_in` (`cage.rs:322`) does full file discovery, but choosing *which
name* (`--cage` → `ISOL8_CAGE` → `config.cage` → default) and the CLI-wins overlay
merge live in `apply_cage_to_opts` (`cli/mod.rs:936-982`).

**G5 — `--analyze` has no single entry point.**
Every piece is public, but the orchestration — feed-vs-live selection, post-run
per-pid feed fallback, macOS `observe_denials_during` wiring with re-spawn fallback,
`--author` trace injection into `profile.macos.raw` — is private in `analyze_cmd`
(`cli/mod.rs:1532-1627`) and `collect_denials_live` (`:1630-1682`).

**G6 — No machine-readable output anywhere.**
`DryRun` (`sandbox.rs:278`) and `EffectivePolicy` (`resolve.rs:35`) have **no derives
at all**. `AnalysisReport`, `DetectResult`, `VerifyResult`, `HomePlan`, `Cage`,
`CapturedRun` are `Debug`/`Clone` only. Only `Denial`/`DenialAccess` (the NDJSON wire
format), `Profile`, and the registry types are serde-capable. No `--json` flag
exists. A Python/Node/C# caller must scrape `print_dry_run` text.

**G7 — `Context` cannot be injected into the pipeline.**
`resolve::effective_policy` calls `Context::from_environment()` internally, and the
config dir comes from a process-global `OnceLock` (`context.rs:59-69`). An embedder
cannot resolve a policy against explicit ambient state. Noted as a Phase 2
follow-up; now blocking (see §3).

**G8 — Nothing is `#[non_exhaustive]`.** Zero occurrences in the workspace. `Spec`
has 15 public fields and is built by struct literal in `tests/`; `Error` has 9
matchable variants. Every future field or variant is a breaking change for
embedders — fix before the API is advertised, not after.

**G9 — The advertised engine-only build is never compiled.**
`justfile:50-54` runs clippy `--all-features` and build/test with default features.
Zero occurrences of `--no-default-features` anywhere in the repo. Worse,
`.github/workflows/ci.yml:28,39` runs clippy and test **without `--workspace`**, and
`Cargo.toml:8` sets `default-members = ["."]` — so core's unit tests never run in
GitHub CI.

**G10 — Engine tests reach into the CLI.**
`tests/profile_filters.rs:4` uses `isol8::cli::{self, ProfileOpts}` across 9 call
sites to build a `Spec` for 18 *engine* filter tests — pulling clap into engine
coverage and guaranteeing `cargo test --no-default-features` cannot compile.

**G11 — Docs are not embedder-grade.**
- Exactly **one** doc example in all of `isol8-core` (`sandbox.rs:361`), plus one in
  `src/lib.rs:9` — both `no_run`, so neither executes.
- No `examples/*.rs`; `examples/` holds a profile TOML.
- `isol8-registry/src/lib.rs` lacks `#![warn(missing_docs)]` (core and cli have it).
- No `[package.metadata.docs.rs]` → docs.rs builds default features only, and gated
  items carry no `doc_cfg` badge, so `isol8::registry` / `isol8::cli` appear and
  vanish with no explanation.
- `sandbox.rs:23` has a **dangling intra-doc link** to
  `crate::cli::ProfileOpts::into_spec` — `isol8-core` has no `cli` module.
- [`../project-structure.md`](../project-structure.md) §3 documents signatures that
  do not exist: `home::resolve(run: &RunArgs, layers: &[ProfileLayer])` (actual:
  `resolve(&Spec, &[Profile], &Context)`), `EffectiveHome { path, seed }` (actual
  adds `real_home`, `plan`), and a `Backend` trait with `probe()` / `Caps` that were
  never written while omitting `Backend::output` that exists.
- [`../instructions.md`](../instructions.md) lists 10 builder methods; `Sandbox` has
  17. Nothing documents cages, recipes, detect/verify, or analyze as library APIs —
  even though `tests/` exercises all of them that way.
- `Backend` cannot be implemented externally (`SandboxChild` constructors are
  `pub(crate)`) — correct, but undocumented, so it reads as an extension point.

---

## 2. Steps

### Step 1 — `isol8-core::config`, one implementation

New `crates/isol8-core/src/config.rs`. Move the *policy* half of
`crates/isol8-cli/src/cli/config.rs`:

```rust
pub struct Config {
    pub default_profiles: Vec<String>,
    pub auto_profiles: bool,
    pub profile_paths: Vec<String>,
    pub add_dirs_rw: Vec<String>,
    pub add_dirs_ro: Vec<String>,
    pub home: Option<String>,
    pub cage: Option<String>,
    pub dry_run: bool,
    pub registries: BTreeMap<String, toml::Value>,  // typed by isol8-registry
}
impl Config { pub fn builtin_defaults() -> Self }

pub fn load() -> Result<Config>;                  // ambient
pub fn load_in(ctx: &Context) -> Result<Config>;  // hermetic — cwd + env from Context
pub fn effective_config_dir() -> PathBuf;         // the single implementation
pub fn init_template(format: &str) -> Result<String>;
```

Reuse `context::expand_at_path` (`context.rs:228`) and `context::absolute_path`
(`:83`) — already in core.

- `registries` stays an untyped `toml::Value` map so core keeps **no** dependency on
  `isol8-registry`; `parse_registries_from_toml` (`registry lib.rs:1528`) still owns
  the typed shape.
- YAML behind a core feature `yaml` (default-on via the facade), so
  `default-features = false` stays lean.
- **Delete** `isol8-registry/src/lib.rs:439-620` and re-export core's. Keep the old
  names as `pub use` so `isol8::effective_config_dir` etc. stay valid.
- **Delete** `context::set_config_dir_provider` / `ConfigDirProvider` and its
  `OnceLock` (`context.rs:57-69`) — core now knows its own config dir.
  `ensure_registry_provider()` (`src/lib.rs:79-88`) keeps only the recipe-dir hook.
- `crates/isol8-cli/src/cli/config.rs` shrinks to clap glue: `apply_to_run`,
  `apply_env_overrides`, `default_init_path`.

### Step 2 — precedence + cage merge on `Spec`, not on `ProfileOpts`

```rust
// core config.rs — engine-side env overrides (was cli/config.rs:433-472)
pub fn apply_env_overrides(cfg: &mut Config);

// core cage.rs — was cli/mod.rs:955-981
pub fn apply_overlay(overlay: &CageOverlay, spec: &mut Spec);
pub fn select_name(flag: Option<&str>, cfg: &Config) -> Option<String>;

// core resolve.rs — the config.md §7 chain, in one place
pub fn spec_from_config(cfg: &Config, cmd: Vec<String>, ctx: &Context) -> Result<Spec>;
```

`prepare_run` / `prepare_opts` become: parse argv → snapshot which fields the user
set → `resolve::spec_from_config` → overwrite with the set fields.

Also make `resolve::parse_set_env` (`resolve.rs:199`) public — `env::build_minimal`
is public and takes `&[(String, String)]`, but the only parser for `"K=V"` (with its
split-once and `=val` rejection semantics) is private.

### Step 3 — `Context` injection through the pipeline (G7)

```rust
pub fn effective_policy_in(spec: &Spec, ctx: &Context) -> Result<EffectivePolicy>;
pub fn effective_policy(spec: &Spec) -> Result<EffectivePolicy>;  // ambient wrapper
```

Same `_in` variant for `sandbox::dry_run`, plus `Sandbox::context(ctx)`. `Context`
already carries `real_home`, `cwd`, `platform`, `config_dir`, `managed_root` with
public fields — this only threads it instead of re-reading the environment
mid-pipeline. Required for hermetic tests, cross-platform resolution, and
in-process Windows embedding (§3).

### Step 4 — one analyze entry point (G5)

```rust
pub struct AnalyzeOutcome { pub code: i32, pub pid: u32, pub report: AnalysisReport }
pub fn run_and_analyze(spec: &Spec, ctx: &Context) -> Result<AnalyzeOutcome>;

#[cfg(target_os = "macos")]
pub fn author_trace(profile: &mut Profile, trace_path: &Path);
```

Body is `collect_denials_live` + `analyze_cmd` minus printing. The CLI keeps
`report.render()` and the `--author` file report.

### Step 5 — serde + `--json` (G6)

Derive `Serialize` on `DryRun`, `EffectivePolicy`, `LayerOrigin`, `EffectiveHome`,
`HomePlan`, `PlannedOp`, `PlanAction`, `HomeOpKind`, `HomeOpSpec`, `AnalysisReport`,
`AnalysisItem`, `RecipePathIndex`, `DetectResult`, `VerifyResult`, `CapturedRun`,
`Cage`, `CageOverlay`, `CageDir`, `HomeMode`, `Recipe`, `RecipeContribution`,
`StrategyName`. Give `DryRun` and `EffectivePolicy` `Debug` + `Clone` while there.

Add `--json` to `--show-policies` / `--dry-run`, `@cage detect`, `@cage verify`,
`@cage show`, `@cage new|edit --preview`, `--analyze`, `@registry list|show|verify`,
`@profiles-list`, `@profiles-show`. One `serde_json::to_string_pretty` branch per
command; text rendering unchanged when the flag is absent.

This is the interface for non-Rust hosts, and it is how the **wizard steps** reach
other programs while the interactive wizard stays in the CLI:
`@cage new --preview --json` emits the rendered cage body, managed hash, drift
status, and security notes as data.

### Step 6 — reach the wizard without clap

`crates/isol8-cli/src/wizard.rs` imports neither clap nor dialoguer; the interactive
loop is `cage_wizard_interactive` (`cli/mod.rs:1272-1435`) and **stays there**. Only
the feature gate is wrong:

```toml
# crates/isol8-cli/Cargo.toml
clap       = { version = "4", features = ["derive"], optional = true }
dialoguer  = { version = "0.11", optional = true }
serde_yaml = { version = "0.9", optional = true }
[features]
clap-cli = ["dep:clap", "dep:dialoguer", "dep:serde_yaml"]
```

```rust
// crates/isol8-cli/src/lib.rs
#[cfg(feature = "clap-cli")] pub mod cli;
pub mod wizard;                       // always
```

```toml
# root Cargo.toml
wizard = ["registry", "dep:isol8-cli"]
cli    = ["wizard", "isol8-cli/clap-cli"]
```

Also flip `mod config;` / `mod diag;` (`cli/mod.rs:512-513`) to `pub mod` so
`@diag`'s `ddmin` minimizer stops being unreachable API (G1).

### Step 7 — forward-compatibility before advertising (G8)

`#[non_exhaustive]` on `Spec`, `Error`, `DryRun`, `EffectivePolicy`, `Context`,
`DetectResult`, `VerifyResult`, `AnalysisReport`, `Cage`, `CageOverlay`. Add
`Spec::new(cmd)` so struct-literal construction is no longer required (what `tests/`
does today). Without this, every field added in Steps 1–5 — and the Windows hook
field in §3 — is a semver break.

### Step 8 — CI gates + honest tests (G9, G10)

```
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check  -p isol8 --no-default-features
cargo check  -p isol8 --no-default-features --features registry
cargo check  -p isol8 --no-default-features --features wizard
cargo build  --workspace
cargo test   --workspace
```

`.github/workflows/ci.yml:28,39` — add `--workspace` so core's unit tests run in CI.
Rewrite `tests/profile_filters.rs` to build `Spec` directly (Step 7 gives it
`Spec::new`), removing the `isol8::cli` import.

---

## 3. Windows embedding + injected-DLL enforcement

`isol8-winhook` does not exist; `backends/windows.rs:99-103` only reserves the NDJSON
denial path, and [`windows-review.md`](./windows-review.md) §5 records that path
grants are **documentary only** on AppContainer. The requirement here is not to build
the hook — it is to keep the API from Steps 1–8 from breaking when the hook lands,
and to state plainly what an embedder gets today.

**R-W1 — The DLL is a build artifact, not a crate item.** A host doing
`cargo add isol8` receives no `isol8_winhook.dll`. Resolution order must be explicit:
`Spec.win_hook_dll` (added under Step 7's `#[non_exhaustive]`) → `ISOL8_WINHOOK_DLL`
→ alongside `std::env::current_exe()` → `%LOCALAPPDATA%\isol8\bin`.

**R-W2 — Never silently downgrade.** If the hook is required by policy and cannot be
loaded, fail with a typed `Error::HookUnavailable`, not an unenforced run
(AGENTS.md: *"no silent loss of confinement"*). Until then, `--show-policies` and the
`--json` `DryRun` must both carry an explicit `enforced: false` / `NOT ENFORCED`
marker for Windows path grants — Step 5 is the right moment to add it.

**R-W3 — Architecture-matched DLLs.** An x64 host may spawn an x86 or ARM64EC child.
Ship `isol8_winhook_{x64,x86,arm64}.dll` and select after `CREATE_SUSPENDED` via
`IsWow64Process2`. The injection sequence (create-suspended → inject → resume) lives
entirely inside `WindowsBackend::spawn` — no public API change, which holds only
while `SandboxChild`'s constructors stay `pub(crate)` (they do).

**R-W4 — The host may itself be a DLL.** For an in-process host (.NET / Electron /
C++ addin), process-global state is a hazard:

- Library paths must never call `std::process::exit`. Today it appears only in
  `cli/mod.rs` (`parse_meta:378`, `run_cmd`, `cage_verify_cmd`); Steps 2 and 4 must
  not carry it into core.
- `OnceLock` providers are first-wins and process-wide. Step 1 deletes
  `set_config_dir_provider`; the remaining `set_offline_registry_provider`
  (`recipe.rs:838`) must be documented as *process-global, first call wins* — two
  hosts in one process cannot disagree.
- **`Context` injection (Step 3) is load-bearing.** Reading `ISOL8_*` and `HOME` from
  the process environment is wrong when that environment belongs to the host.
  `effective_policy_in(&spec, &ctx)` + `config::load_in(&ctx)` make the whole
  pipeline drivable from explicit state.

**R-W5 — Non-Rust hosts: subprocess first.** The `--json` surface (Step 5) plus
`SandboxChild::{id, wait, kill}` covers C# / Node / Python without an FFI boundary.
A `crates/isol8-ffi` `cdylib` with a C ABI over the same JSON
(`isol8_run_json(*const c_char, *mut *mut c_char) -> i32`) is a later deliverable,
justified only if a host genuinely cannot spawn a process. **Not in this plan.**

**R-W6 — State the truth.** Embedding on Windows today gives process isolation
(AppContainer draft) and **no path enforcement**.
[`../windows-support.md`](../windows-support.md) and the new embedding guide must say
so plainly rather than listing Windows as a supported target.

---

## 4. Documentation + examples

**New `_docs/embedding.md`** — the API contract, which
[`../instructions.md`](../instructions.md) then links to: feature matrix
(`default-features=false` / `registry` / `wizard` / `cli`) with the dependency cost
of each; the pipeline from `Config` → `Spec` → `EffectivePolicy` → `HomePlan` →
spawn; a task-oriented section per capability (*load config*, *resolve a cage*,
*enumerate recipes*, *detect toolchains*, *create a managed home*, *dry-run a
policy*, *run and analyze*, *author a cage*); the `--json` schemas; the `Context`
injection story; §3 above; and the non-extension-points (`Backend` cannot be
implemented externally).

**New `examples/*.rs`**, each runnable via `cargo run --example`:

| Example | Features | Shows |
|---|---|---|
| `embed_minimal.rs` | `default-features = false` | `Sandbox::new().profile().grant_rw().run()` — doubles as the engine-only compile gate |
| `embed_config.rs` | `registry` | `config::load()` → `spec_from_config` → `dry_run` — the CLI's behaviour, headless |
| `embed_cage.rs` | `registry` | `cage::resolve_in` → `apply_overlay` → run |
| `embed_recipes.rs` | `registry` | `RecipeRegistry::ids()` → `detect_all` → `HomePlan::compute`/`apply` |
| `embed_analyze.rs` | `registry` | `run_and_analyze` + NDJSON feed fallback |
| `embed_wizard.rs` | `wizard` | `WizardRequest` → `render` → `check_drift` → `apply` |
| `embed_json.rs` | `default-features = false` | serialize `DryRun` — what a non-Rust host parses |

Wire into `just ci` (`cargo build --examples --workspace`) so they cannot rot.

**Rustdoc:** `#![warn(missing_docs)]` on `isol8-registry`;
`[package.metadata.docs.rs] all-features = true` +
`#![cfg_attr(docsrs, feature(doc_cfg))]` and `#[cfg_attr(docsrs, doc(cfg(…)))]` on
the facade's gated items; fix the dangling link at `sandbox.rs:23`; module-level
`//!` docs on every core module with a worked example for `config`, `cage`,
`recipe`, `detect`, `analyze`, `plan`; add
`cargo doc --no-deps --workspace -D warnings` to `just lint`.

**Existing docs to correct:** [`../project-structure.md`](../project-structure.md) §3
(real signatures; add `config.rs` to tree + data flow), [`../config.md`](../config.md)
(`Config` is now a core type), [`../instructions.md`](../instructions.md) (17 builder
methods, link to embedding.md), [`multi-evo-plan.md`](./multi-evo-plan.md) (record
that the *no-policy-in-CLI* rule was violated by `cli/config.rs` + `prepare_run` and
is restored here), `README.md`, `AGENTS.md`.

---

## 5. Verification

```sh
just ci                            # incl. the three --no-default-features gates
cargo build --examples --workspace
cargo doc --no-deps --workspace    # warning-free
just field-test                    # Steps 3-4 touch the spawn path
```

**Behaviour parity** — capture before, diff after; must be byte-identical:

```sh
isol8 --show-policies claude --version
isol8 -c work --show-policies -- echo hi
ISOL8_CONFIG_PATH=./_data/config isol8 --show-policies -- echo hi
isol8 @cage detect
isol8 @registry list
```

**New surface:**

```sh
cargo run --example embed_config   # same layers/grants as --show-policies above
cargo run --example embed_recipes  # creates a managed home under ./_data/config/homes/
isol8 --show-policies --json echo hi | jq -e '.layer_names, .profile.paths, .home_plan'
isol8 @cage detect --json | jq -e '.[0].id'
```

**Regression watch:** `tests/registry.rs` and `tests/recipe.rs` cover the config-dir
and offline-discovery paths that Step 1 rewrites — they must pass unchanged, and
`_data/config` (via the repo's `.isol8.toml` `config_path`) must still resolve cages,
`@managed/<id>` homes, and `@…` profile paths under that tree.

---

## 6. Out of scope

- Building `isol8-winhook` (the DLL) or fixing the AppContainer blockers in
  [`windows-review.md`](./windows-review.md).
- `crates/isol8-ffi` C ABI (R-W5).
- Lifting `@registry` install/update orchestration or `@diag`'s `ddmin` into library
  API — the primitives are already public and the orchestration stays in the CLI.
  Step 6 makes both modules `pub` so they are at least reachable.
- Evolution Phase 10 (Linux shadow observe), network tiers, registry signing, full TUI.

---

## Changelog

| Date | Change |
|------|--------|
| 2026-07-28 | Initial plan from the embedding-API audit (G1–G11); steps 1–8 + Windows requirements |
| 2026-09-02 | Host-integration guide [`../integration.md`](../integration.md) + `examples/embed_harness.rs`. Writing it found a hermeticity leak: the automatic cwd grant read `std::env::current_dir()` even on the `_in` path, so an embedding host's own directory was granted rw in every confined session. `profile::load_merged` now takes the `Context` and `overrides_layer` the cwd from it (unit + integration test). The remaining ambient read (user-config layer/recipe overlays) is documented rather than changed |
| 2026-09-02 | Implementation audit against the plan: Steps 1–6, 8 and the docs verified in tree; Step 7 completed (`#[non_exhaustive]` on the engine-produced types, `Context` / `CageOverlay` intentionally left open) and `embedding.md` §6 corrected to state the real contract |
