# isol8 — Multi-phase Evolution Plan

**Status:** active plan — Phase 0–9 complete; next is Phase 10 (Linux `--analyze` / shadow, **deferred**)  
**Source proposal:** [`_docs/inbox/evo-repo.md`](../inbox/evo-repo.md)  
**Related prior art:** [`_docs/inbox/home-config-wizard.md`](../inbox/home-config-wizard.md) (exposure modes ≈ strategies)  
**Companion docs:** [`profile-model.md`](../profile-model.md), [`project-structure.md`](../project-structure.md), [`AGENTS.md`](../../AGENTS.md)  
**Target:** post-0.2.x (does not block Phase 1 MVP / network tiers)

---

## How to use this document

1. **Execute one phase at a time.** Do not start phase *N+1* until phase *N* is
   marked **done** and the gate has passed.
2. **Gate after every phase:** `just ci` (or `cargo fmt --check` +
   `cargo clippy -- -D warnings` + `cargo build` + `cargo test`). Field tests
   (`just field-test`) when the phase touches enforcement or materialization.
3. **Update this file when a phase finishes** — fill *Resume notes* with concrete
   paths, type names, decisions taken, and leftover follow-ups so work can restart
   cold from this document alone.
4. **Keep `_docs/*` in sync** with what landed (see each phase’s *Docs* checklist).
5. **Do not invent scope.** Open decisions stay open until explicitly resolved
   here or in a linked decision note. Prefer asking over guessing (AGENTS.md).

### Progress board

| Phase | Deliverable | Status | Gate |
|------:|-------------|--------|------|
| 0 | Analysis + this plan | **done** | n/a (docs only) |
| 1 | Cages (selection → Spec) | **done** (2026-07-23) | `just ci` |
| 2 | Context + home materialization (plan/apply) | **done** (2026-07-23) | `just ci` + field 17–19 |
| 3 | Recipes + strategies | **done** (2026-07-24) | `just ci` |
| 4 | Detection + `@cage verify` | **done** (2026-07-24) | `just ci` |
| 5 | Shared analysis layer + Windows `--analyze` | **done** (2026-07-24) | `just ci` |
| 6 | macOS `--analyze` | **done** (2026-07-24) | `just ci` + live smoke |
| 7 | Registry (local/git/http + lockfile) | **done** (2026-07-24) | `just ci` |
| 8 | Wizard (`@cage new/edit`) | **done** (2026-07-24) | `just ci` |
| 9 | Crate split (`isol8-core` / `-registry` / `-cli`) | **done** (2026-07-24) | `just ci` |
| 10 | Linux `--analyze` (shadow mode) | deferred | — |

Phases map 1:1 to evo-repo §10 with an explicit Phase 0 for planning and
documentation hygiene.

---

## 0. Analysis summary (current → proposed)

### 0.1 What isol8 already has (foundation)

Phase 1 engine is a solid **policy engine**:

| Capability | Location |
|------------|----------|
| Profile layers, deny-first merge, `requires` | `src/profile.rs` |
| Filters (os / arch / executables) | `src/filter.rs` |
| HOME-before-grants, `#HOME` / `~`, seed-ro | `src/home.rs`, `resolve::effective_policy` |
| Sanitized env | `src/env.rs` |
| Spec / Sandbox / DryRun library API | `src/sandbox.rs` |
| Layer overlay: builtin → user config → profile-path | `LayerRegistry` |
| Meta-commands (`@init`, `@profiles-*`, `@diag`) | `src/cli/` |
| macOS Seatbelt + Linux Landlock + Windows draft | `src/backends/` |

**Invariant that must never break:** effective `$HOME` is resolved **before** any
path-grant expansion (`resolve.rs` → `home::resolve` then `load_merged`).

### 0.2 What the proposal adds (configuration & lifecycle)

evo-repo does **not** replace the enforcement core. It adds a layer above it:

```
Cage (local selection)
  → Recipes (shared: grant + materialization + env + strategy)
  → Registry (out-of-band distribution)
  → Plan/apply (previewable mutations)
  → Analyze / wizard (feedback loop)
  → existing Spec + effective_policy + backends
```

Key glossary (authoritative in evo-repo §2):

| Term | Role |
|------|------|
| **Cage** | Local named isolation unit (home mode + profiles + strategy choices) |
| **Profile** | Existing raw policy layer — unchanged |
| **Recipe** | Profile + detect / strategies / home ops / verify |
| **Bundle** | Curated recipe set for common setups |
| **Registry** | Source of profiles/recipes/bundles (git/http/dir), offline-by-default |
| **Strategy** | `share` \| `link` \| `isolate` — per-tool materialization + grants |
| **Materialization** | FS state under replaced home (`link`/`mkdir`/`seed-ro`/`copy`) |
| **Plan/apply** | Mutations computed first, applied second |

### 0.3 Gap matrix (condensed)

| Concept | Today | Gap |
|---------|-------|-----|
| Cages | none | new document + resolve order + CLI |
| Recipes / strategies | path-only toolchain profiles | new schema, compile → Profile + HomePlan |
| Registry | `LayerRegistry` in-process only | trait + backends + lockfile |
| Materialization | `home::seed` (copy-ro, first-create) | link/mkdir/copy + plan/apply |
| Context | ambient `HOME` / `RunContext` | injectable `Context` |
| Analyze | `@diag` (launch SBPL delta-debug) | runtime denials → recipe suggestions |
| Wizard | `@cage new`/`edit` (Phase 8) | full TUI / clone / fix (deferred) |
| Crate split | single crate + `cli` feature | **done** (Phase 9 workspace) |

### 0.4 Design principles for implementation

1. **Compile into Spec, don’t fork the pipeline.** Cage/recipe resolution ends in
   the existing `Spec` → `effective_policy` → spawn path.
2. **Recipes are not free-form Profile extensions** if that breaks
   `deny_unknown_fields`. Prefer a separate document type that *compiles down* to
   `Profile` + `HomePlan` + env.
3. **Offline by default.** Registry fetch only on explicit `@registry update`.
4. **Plan before apply.** Dry-run / wizard preview / verify share one mutation path.
5. **Deny-by-default stays.** No implicit widening of grants when adding recipes.
6. **KISS.** Smallest shippable phase; no speculative abstractions (AGENTS.md).

### 0.5 Risks that constrain ordering

| Risk | Mitigation in plan |
|------|-------------------|
| Symlink semantics differ across Seatbelt / Landlock / Windows | Field-test links in Phase 2 before trusting strategies (Phase 3) |
| Windows symlinks need Developer Mode | Junction fallback decision before heavy Win analyze |
| Recipe commands (`detect`/`verify`) = larger blast radius | Gate trust; Phase 4+ only |
| Env values not tilde-expanded today | Fix when recipes land (Phase 3) |
| Crate split too early freezes bad APIs | Phase 9 last among core features |

---

## Phase 1 — Cages (selection layer → Spec)

**Status:** **done** (2026-07-23)  
**Depends on:** Phase 0  
**evo-repo:** §3 (without toolchains/strategies), §10 step 1  
**Goal:** One-knob invocation (`isol8 -c work claude`) with **no** new enforcement.

### In scope

- Cage TOML schema `schema = 1`:
  - `name`
  - `home`: `inherit` | absolute/relative path | (optional stretch) `ephemeral`
  - `profiles = [...]`
  - `[[dirs]]` → `path` + `access` (`ro`/`rw`)
  - **Defer** `[toolchains.*]` (ignore with warning if present, or reject cleanly)
- Resolution order (first hit wins for *which cage*, not merge):
  1. `ISOL8_CAGE`
  2. `--cage` / `-c`
  3. `./isol8.toml` `[cage]` section (name or inline)
  4. `./.isol8/cage.toml`
  5. walk up to git root, retry 3–4
  6. `~/.config/isol8/cages/default.toml`
- CLI: `--cage`/`-c`, env `ISOL8_CAGE`
- Meta: `@cage list`, `@cage show <name>` (read-only)
- Optional non-interactive `@cage new <name> --home inherit|path` (write template only)
- Map cage → Spec fragment: `profiles`, `home`, `add_dirs_*`
- **Precedence:** existing CLI flags override cage (no breaking change)

### Out of scope

- Strategies, materialization beyond current seed, registry, wizard UX, analyze

### Implementation sketch

| Area | Action |
|------|--------|
| New | `src/cage.rs` — parse, discover, resolve name → `Cage`, `Cage::to_spec_overlay()` |
| Spec | Optional `cage: Option<String>` on `Spec` **or** resolve cage *before* building Spec in CLI only (prefer CLI resolve → fill Spec fields; keep Spec clap-free and cage-agnostic if possible) |
| CLI | `ProfileOpts` + `parse_meta` for `@cage *` |
| Config | Optional `[cage]` / `cage = "work"` in `isol8.toml` |
| Errors | typed: cage not found, invalid schema, ambiguous path |

**Recommendation:** resolve cage in CLI/config layer into existing `Spec` fields so
the library API stays stable. Library users build Spec directly (evo-repo §7.3
struct path can wait).

### Tests

- Unit: discovery order (temp dirs), TOML parse, flag override of cage home/profiles
- Integration: `isol8 -c work --show-policies echo hi` shows expected layers + dirs
- Negative: missing cage file, bad schema → clear errors

### Docs checklist

- [x] `_docs/instructions.md` — `--cage`, resolution, examples
- [x] `_docs/project-structure.md` — `cage` module in layout + data flow
- [x] `_docs/profile-model.md` — note cages as selection layer (not profiles)
- [x] `AGENTS.md` — current status bullet
- [x] This plan — status + resume notes

### Gate

```sh
just ci
```

### Resume notes

**Completed 2026-07-23.** Restart from Phase 2; cage selection is stable.

#### Files added/changed

| Path | Role |
|------|------|
| `src/cage.rs` | Parse/validate cage TOML; `HomeMode`; discovery; `list`/`write_new`/`format_show`; unit tests |
| `src/sandbox.rs` | `Spec.ephemeral_home: bool` |
| `src/home.rs` | `ephemeral_home` → scratch (same as `auto_scratch`) |
| `src/lib.rs` | `pub mod cage`; re-export `Cage`, `CageOverlay`, `HomeMode` |
| `src/cli/mod.rs` | `--cage`/`-c`, hidden `--ephemeral-home`, `@cage list\|show\|new`, `apply_cage_to_opts` |
| `src/cli/config.rs` | `Config.cage: Option<String>`; init template comment |
| `tests/cage.rs` | Integration: overlay → Spec, project discovery |

#### Behaviour locked

1. **CLI resolve → Spec** — cage never appears on `Spec`; only filled fields (`profiles`, `home`, `ephemeral_home`, `add_dirs_*`).
2. **Name order:** CLI `-c` / `--cage` → `ISOL8_CAGE` → config `cage =` → default discovery (`.isol8/cage.toml` walk-up → user `default.toml`).
3. **File discovery for named cage:** project `.isol8/cages/{name}.toml`, `.isol8/{name}.toml` (walk to git root), then `~/.config/isol8/cages/{name}.toml`. Bare path or `*.toml` path also accepted.
4. **Merge precedence:** CLI-set profiles/home/dirs win; cage fills empties; then config defaults; then `ISOL8_*` env overrides (existing env behaviour unchanged).
5. **Empty `profiles = []`** does not override config `default_profiles`. Non-empty **replaces** them (include `macos/system-runtime` yourself if needed).
6. **`home`:** `inherit` | `ephemeral` | path. `@managed/*` **errors** with Phase 2 pointer.
7. **`[toolchains.*]`** accepted in TOML but ignored with stderr warning (Phase 3).
8. **`[[dirs]]`** access only `ro`/`rw`.

#### Public API impact

- New engine module `isol8::cage` (available with `default-features = false`).
- `Spec.ephemeral_home` (default `false`) — additive, non-breaking.
- Config optional field `cage` — additive.

#### Known follow-ups (not blockers)

- Inline `[cage]` table *inside* `isol8.toml` (full cage body) not implemented — only `cage = "name"`.
- No `@cage edit` / clone / verify (later phases).
- Walk-up stops at `.git`; no pure-filesystem unlimited walk beyond that once `.git` is hit (ancestors above git root are not searched for project cages).
- Symlink / materialization semantics still untested (Phase 2).

---

## Phase 2 — Context + home materialization (plan / apply)

**Status:** **done** (2026-07-23)  
**Depends on:** Phase 1  
**evo-repo:** §4.2, §7.2, §7.4, §10 step 2  
**Goal:** Replaced homes can be *prepared* idempotently; dry-run shows the plan.

### In scope

- Injectable **`Context`**:
  ```text
  real_home, cwd, platform, managed_root
  ```
  CLI calls `Context::from_environment()`; tests inject hermetic values.
- Home modes fully:
  - `inherit` — real home (no replacement)
  - `@managed/<id>` — under platform data dir / `managed_root`
  - `ephemeral` — temp dir (existing scratch behavior)
  - absolute path still allowed; lint later
- **`HomeOp`** primitives: `link`, `mkdir`, `seed-ro` (existing seed), `copy`
- **`HomePlan`**: `compute(ops, ctx) -> Plan` (no side effects); `apply(plan)`
- Wire plan/apply into spawn path (replace direct-only `home::seed` or make seed
  a HomeOp)
- Dry-run / `--show-policies` prints planned home ops (structured field on `DryRun`)
- Idempotent ops; first-creation-only for seed-ro remains

### Out of scope

- Recipe strategies (Phase 3) — but ops API must be strategy-ready
- Network registry

### Implementation sketch

| Area | Action |
|------|--------|
| New | `src/context.rs` or extend `home.rs` carefully |
| New | `HomePlan` / `HomeOp` types; plan/apply |
| home.rs | `@managed` resolution; refactor `seed` into ops |
| resolve / sandbox | call plan; apply only on spawn (not dry-run) |
| Field tests | new scenarios: link + mkdir under `--home` |

### Critical verification (before Phase 3)

Document results in resume notes:

1. Seatbelt: grant on symlink path vs target — what is required?
2. Landlock: same
3. Windows: symlink vs junction availability without elevation

If links are unreliable, Phase 3 strategies that depend on `link`/`share` must
grant **both** path forms or use copy fallbacks.

### Tests

- Unit: plan contents, idempotent apply twice, managed path construction
- Field: materialize link then confined process can read through it (per OS)

### Docs checklist

- [x] `profile-model.md` — home modes + materialization primitive (even if only
      driven by cage/recipe, not raw profile yet)
- [x] `project-structure.md` — Context, HomePlan in data flow
- [x] `instructions.md` — `@managed`, ephemeral, dry-run home section
- [x] `testing-strategies.md` — new field scenarios
- [x] This plan — symlink findings

### Gate

```sh
just ci
just field-test   # if materialization field scenarios landed
```

### Resume notes

**Completed 2026-07-23.** Restart from Phase 3 (recipes + strategies).

#### Symlink semantics (field-test 18–19, macOS)

| Grant surface | Read through link → real target? |
|---------------|----------------------------------|
| Target (`#HOME/.tool`) + effective home | **Yes** (scenario 18 PASS) |
| Link path only (`~/.tool` under eff home) | **No** — Seatbelt denies (scenario 19: *link-path grant INSUFFICIENT*) |

**Implication for Phase 3 strategies:** `link` / `share` recipes must emit path
grants on the **real target** (`#HOME/…`), not only on the symlink path in the
replaced home. Dual grants (link path + target) are safest.

**Linux:** not re-measured in this session; expect Landlock PathBeneath on the
link path to *not* cover the real target either — same dual-grant rule.
**Windows:** symlink create may require Developer Mode; field scenarios skip path
enforcement; junction fallback still open (§9.3).

#### Files added/changed

| Path | Role |
|------|------|
| `src/context.rs` | `Context`, `Platform`, `from_environment`, `managed_home`, `default_managed_root` |
| `src/plan.rs` | `HomeOpSpec`/`HomePlan`/`PlannedOp`; compute/apply/render; seed_specs_from_list |
| `src/home.rs` | `resolve(spec, layers, ctx)`; `@managed/`; `EffectiveHome.{real_home,plan}`; `materialize` |
| `src/sandbox.rs` | `Spec.home_ops`; `DryRun.{home_plan,home_path}`; builder `home_op`/`ephemeral_home` |
| `src/resolve.rs` | builds `Context::from_environment()` before home resolve |
| `src/cli/mod.rs` | dry-run prints home plan; spawn uses `materialize` |
| `src/cage.rs` | `@managed/<id>` accepted as home path |
| `src/bin/isol8-field-test.rs` | scenarios 17–19 |

#### Behaviour locked

1. **Plan before apply** — dry-run / `--show-policies` never mutates the filesystem.
2. **Spawn path** — `home::materialize` (= `HomePlan::apply`) before confine/spawn.
3. **Seed → seed-ro** — profile `home_replace.seed` becomes seed-ro ops; first-creation-only.
4. **Replacement home mkdir** — when effective path ≠ real home, plan starts with mkdir of the home path.
5. **`@managed/<id>`** — under `Context.managed_root` (`$XDG_DATA_HOME/isol8/homes` or `~/.local/share/isol8/homes` on Unix).
6. **Tokens** — `~` effective home, `#HOME` real home, `@managed/id` managed root (plan + home resolve).
7. **Spec.home_ops** — embedder/recipe feed; CLI does not yet parse ops from cage files.

#### Public API

- `isol8::{Context, Platform, HomePlan, HomeOpSpec, HomeOpKind, PlannedOp, PlanAction}`
- `Spec.home_ops`, `Sandbox::home_op`, `Sandbox::ephemeral_home`
- `DryRun.home_plan`, `DryRun.home_path`
- `home::resolve` now takes `&Context` (breaking for direct callers; only internal + tests)

#### Known follow-ups

- Recipes compile strategies into `home_ops` + path grants (Phase 3) — dual-grant targets.
- Windows junction fallback when symlink fails.
- Linux field re-run for scenarios 18–19 to confirm Landlock matches Seatbelt.
- Optional: inject `Context` through `effective_policy` for hermetic library tests without env.

---

## Phase 3 — Recipes + strategies

**Status:** **done** (2026-07-24)  
**Depends on:** Phase 2  
**evo-repo:** §4, §10 step 3  
**Goal:** One toolchain works under a replaced home via declarative strategy.

### In scope

- Recipe document type (`kind = "recipe"`, `schema = 1`):
  - `id`, `filter`, `summary`
  - `[detect]` optional stub ok if full detect is Phase 4
  - `[strategies.share|link|isolate]` each with `home` ops, `paths`, `env`
  - optional `default_strategy` (open decision — may ship optional field)
- Cage `[toolchains.<id>] strategy = "..."` managed sections (parse + apply;
  wizard ownership of rewrite is Phase 8)
- Compile recipe + chosen strategy →:
  - path grants layer (or synthetic Profile)
  - env map
  - HomePlan ops
- Load recipes from:
  - embedded optional set **or** local dir (e.g. `recipes/` or user config)
  - **not** full git registry yet — local/embedded only
- Env value expansion against Context (`~`, `#HOME`) — fix known gap
- Platform selector: reuse `ProfileFilter`; variant files by convention
- Disjoint-selector validation when multiple variants share an id

### Out of scope

- Full registry install/update, lockfile (Phase 7)
- Interactive wizard (Phase 8)
- verify commands (Phase 4) can parse but not require

### Schema boundary decision (lock in resume notes)

**Preferred:** separate `Recipe` type compiled to `Profile` + ops — do **not**
stuff strategies into `Profile` with `deny_unknown_fields` unless a clean
`kind` discriminator is introduced for dual-purpose files.

### Tests

- Unit: strategy selection, compile to grants/ops, filter variants, env expand
- Integration: cage with `toolchains/nvm` strategy `link` → dry-run shows expected
  paths + plan (use a fixture recipe under `tests/fixtures/`)
- Field: at least one real toolchain recipe if available on CI host (optional)

### Docs checklist

- [x] New `_docs/recipes.md` **or** section in `profile-model.md`
- [x] Example recipes under `recipes/` or docs
- [x] `AGENTS.md` architecture bullet
- [x] This plan

### Gate

```sh
just ci
```

### Resume notes

**Completed 2026-07-24.** Restart from Phase 4 (detect + `@cage verify`).

#### Schema decision

**Separate `Recipe` type** (`src/recipe.rs`) — not fields on `Profile`.
`#[serde(deny_unknown_fields)]` on profile load unchanged. Recipes compile to
`RecipeContribution { home_ops, paths, env }` folded in `resolve::effective_policy`.

#### Files added/changed

| Path | Role |
|------|------|
| `src/recipe.rs` | Parse, `RecipeRegistry`, compile, env expand, disjoint filter lint |
| `recipes/toolchains/{nvm,cargo,maven}.toml` | Builtin recipes |
| `build.rs` | Embeds `recipes/**/*.toml` → `BUILTIN_RECIPES` |
| `src/cage.rs` | Parse `[toolchains.*] strategy`; overlay includes choices |
| `src/sandbox.rs` | `Spec.toolchains`, `recipe_paths`; DryRun.recipes; builder helpers |
| `src/resolve.rs` | Compile recipes → home_ops before home resolve; merge grants/env after |
| `src/cli/mod.rs` | Cage toolchains → Spec; dry-run `-- recipes --` section |
| `tests/recipe.rs` | Integration |
| `_docs/recipes.md` | User/author guide |

#### Behaviour locked

1. Cage key `nvm` → id `toolchains/nvm`; keys with `/` pass through.
2. Strategy required in cage TOML (`strategy = "link"|"share"|"isolate"`).
3. Recipe env values expanded (`~` → effective home, `#HOME` → real).
4. Path grants from recipes use `#HOME/…` for link/share (dual-grant rule from Phase 2).
5. Recipe env is default-only (does not clobber profile/CLI).
6. Unknown recipe / wrong platform / missing strategy → hard error.
7. Disjoint filter validation across variants of one id on registry load.
8. `detect` / `verify` blocks parsed but not executed (Phase 4).

#### Public API

- `isol8::{Recipe, RecipeRegistry, StrategyName, ToolchainChoice}`
- `Sandbox::toolchain(id, strategy)`, `Sandbox::recipe_path`
- `Spec.toolchains`, `Spec.recipe_paths`
- `DryRun.recipes`, `EffectivePolicy.recipes`

#### Known follow-ups

- Phase 4: run `detect.probe` / `verify.cmd`
- Phase 7: remote registry (git/http) + lockfile
- Windows nvm variant if needed; cargo on windows
- Optional CLI flag `--toolchain nvm:link` without cage

---

## Phase 4 — Detection + `@cage verify`

**Status:** **done** (2026-07-24)  
**Depends on:** Phase 3  
**evo-repo:** §6.2, §6.5, §10 step 4  
**Goal:** Read-only toolchain discovery; prove a cage works with recipe smoke tests.

### In scope

- `detect.probe` (stat path) + optional `detect.version` command
- `@cage detect` — no side effects; print table of found toolchains
- `@cage verify [name]` — materialize plan, run each recipe `verify.cmd` **inside**
  the cage (via isol8 itself)
- Trust gate for version/verify commands from non-builtin sources (simple:
  builtin/local trusted; external deferred to Phase 7)
- Failure output suggests `@cage fix` / manual grant (full analyze later)

### Out of scope

- Interactive multi-step wizard
- Registry-backed recipe download

### Tests

- Unit: probe hit/miss with temp dirs
- Integration: fixture recipe verify success/fail paths
- No network required

### Docs checklist

- [x] `instructions.md` — detect/verify
- [x] `recipes.md` — runtime behaviour
- [x] This plan

### Gate

```sh
just ci
```

### Resume notes

**Completed 2026-07-24.** Restart from Phase 5 (shared analysis + Windows `--analyze`).

#### Files added/changed

| Path | Role |
|------|------|
| `src/detect.rs` | probe/stat, host version, verify orchestration, trust gate, expect matcher |
| `src/sandbox.rs` | `run_captured` / `CapturedRun` |
| `src/backends/{mod,macos,linux}.rs` | `Backend::output` (macOS pipes; Linux fork+pipe) |
| `src/cli/mod.rs` | `@cage detect`, `@cage verify [name]` |
| `src/lib.rs` | `pub mod detect` |
| `tests/detect_verify.rs` | Integration |
| `_docs/{instructions,recipes}.md`, `AGENTS.md` | User-facing docs |

#### Behaviour locked

1. **`detect.probe`** — path `stat` only; `~` / `#HOME` expand against **real** home.
2. **`detect.version`** — optional host command when probe hits and source is trusted.
3. **`@cage detect`** — all platform-matching recipes; read-only; no cage name required.
4. **`@cage verify`** — materialize once, then each `verify.cmd` via `run_captured` (confined).
5. **`verify.expect`** — tiny pattern subset (`^`/`$`/`\d`/`\d+`/`.`); no `regex` crate.
6. **Trust** — `builtin:` and non-URL local paths trusted; `://` / `registry:` blocked for version+verify.
7. **Verify cmd argv** — whitespace split unless shell metacharacters → then `/bin/sh -c`.
8. **Cage resolve for verify** — same discovery as normal; name optional (default cage).

#### Known follow-ups

- Phase 5–6: `--analyze` handoff from verify failures
- Phase 7: registry trust for remote recipes
- `nvm --version` often fails on host PATH (nvm is a shell function) — probe still works
- Windows `output` uses default (no body capture) until AppContainer pipes land

---

## Phase 5 — Shared analysis layer + Windows `--analyze`

**Status:** **done** (2026-07-24)  
**Depends on:** Phase 4 (index of recipe path prefixes useful; can soft-depend on 3)  
**evo-repo:** §8.3–8.4, §10 step 5  
**Goal:** Prove denial → recipe suggestion on the cheapest backend (Windows hook).

### In scope

- Shared types: `Denial { path, access, count, pid, exe }`
- Collapse to roots; match against recipe index prefixes
- Classify “missing materialization” vs “missing grant” (stat real home)
- CLI: `isol8 --analyze <cmd…>`
- Windows: log denials from hook decision point → NDJSON → parent
- Caveat in output: user-mode hooks are non-exhaustive

### Out of scope

- macOS log scrape (Phase 6)
- Linux shadow mode (Phase 10)
- `--author` draft recipes (nice-to-have if cheap)

### Prerequisites / blockers

- Windows backend must actually enforce path policy enough for denials to fire
  (see `_docs/wip/windows-review.md`). If not, implement a **test double** that
  feeds synthetic denials into the shared layer so the engine lands anyway, and
  mark Win hook wiring blocked.

### Tests

- Unit: collapse roots, match index, materialization classification
- Windows integration if CI allows; else unit with recorded NDJSON fixtures

### Docs checklist

- [x] `windows-support.md` — analyze mode
- [x] `instructions.md` — `--analyze`
- [x] This plan

### Gate

```sh
just ci
```

### Resume notes

**Completed 2026-07-24.** Restart from Phase 6 (macOS unified-log scrape).

#### Files added/changed

| Path | Role |
|------|------|
| `src/analyze.rs` | `Denial`, collapse roots, recipe prefix index, needs-home-link, NDJSON I/O, report render |
| `src/cli/mod.rs` | `--analyze` flag; post-run feed load + report |
| `src/backends/windows.rs` | `analyze_denial_log_path` stub for future hook |
| `src/lib.rs` | `pub mod analyze` |
| `tests/analyze.rs` | Integration |
| `Cargo.toml` | `serde_json` for NDJSON |
| Docs | instructions, windows-support, AGENTS, this plan |

#### Behaviour locked

1. **Shared pipeline** — parse NDJSON → collapse to home-child roots → match recipe prefixes → classify home-link.
2. **CLI** — `isol8 --analyze CMD` always spawns (best-effort), then analyzes denials if present.
3. **Feed order** — `ISOL8_ANALYZE_FEED` file, else `$TMP/isol8-analyze-<pid>.ndjson`.
4. **No feed** — empty report + platform-specific note (Win hook deferred; macOS Phase 6; Linux Phase 10).
5. **Windows live hook** — **not wired**; R2 path grants remain documentary. Shared layer proven via NDJSON test double.
6. **Output caveat** — “observed only / non-exhaustive”.
7. **Does not** edit cages or auto-apply recipes.

#### Public API

- `isol8::{Denial, DenialAccess, AnalysisReport}`
- `analyze::{parse_ndjson, load_ndjson_file, collapse_to_roots, build_recipe_index, analyze, …}`

#### Known follow-ups

- Phase 6: macOS `log stream` → same `Denial` records
- Phase 10: Linux shadow mode
- Wire live Windows NDJSON when path hook / R2 exists
- Prefer recipe `default_strategy` when multiple strategies match the same prefix
- Handoff from `@cage verify` failures

---

## Phase 6 — macOS `--analyze`

**Status:** **done** (2026-07-24)  
**Depends on:** Phase 5 (shared layer)  
**evo-repo:** §8.2, §10 step 6  
**Goal:** Denial observation via unified log; optional trace for authoring.

### In scope

- Default: `log stream` / unified log scrape, filter by child PID
- Startup race handling (stream ready before exec)
- Optional `--author`: Seatbelt `(trace …)` permissive profile generation
  (**explicit opt-in only**)
- Reuse shared post-processing from Phase 5

### Out of scope

- Linux

### Tests

- Unit: parse sample log lines → Denial
- Field (macOS): deliberate missing grant → suggestion appears

### Docs checklist

- [x] `macos-support.md` — analyze vs `@diag` (complementary)
- [x] `instructions.md`
- [x] This plan

### Gate

```sh
just ci
just field-test  # if analyze field scenario added
```

### Resume notes

**Completed 2026-07-24.** Evolution track continued through Phase 8 (wizard); next is Phase 9 (crate split).

#### Files added/changed

| Path | Role |
|------|------|
| `src/analyze_macos.rs` | Parse `Sandbox: … deny(…)`, `LogStream`, `log show` fallback, `observe_denials_during`, `(trace …)` helper |
| `src/cli/mod.rs` | Live macOS path in `analyze_cmd` / `collect_denials_live`; `--author` |
| `src/lib.rs` | `#[cfg(macos)] pub mod analyze_macos` |
| Docs | macos-support, instructions, AGENTS, this plan |

#### Behaviour locked

1. **Feed still wins** — `ISOL8_ANALYZE_FEED` skips log stream (offline/CI).
2. **Live path** — start `log stream --style ndjson` → sleep 400ms → spawn → wait → kill stream → drain 600ms → if empty, `log show --last 15s`.
3. **Predicate** — `eventMessage CONTAINS "deny("` (parse filters non-sandbox noise).
4. **PID filter** — prefer matching denial pid; if empty after filter, keep all (sandbox-exec pid mismatch).
5. **`--author`** — requires `--analyze`; appends `(trace "…")` to `profile.macos.raw`; warns permissive; prints output path if written.
6. **Complements `@diag`** — launch abort vs runtime denials.

#### Verified on host (macOS)

```text
isol8 --analyze --profile base --profile macos/system-runtime -- /bin/cat ~/.ssh/id_rsa
→ Observed 2 denials (.ssh r, /dev/dtracehelper w)
```

#### Known follow-ups

- Phase 10: Linux shadow mode
- TCC / Full Disk Access may still empty the stream on locked-down hosts
- Field-test scenario for analyze (optional; live log is environment-dependent)

---

## Phase 7 — Registry (git / http / local + lockfile)

**Status:** **done** (2026-07-24)  
**Depends on:** Phase 3 (schema); Phase 4 informs which recipes to seed officially  
**evo-repo:** §5, §7.5, §10 step 7  
**Goal:** Policy evolution decoupled from the binary; offline cache + pins.

### In scope

- `ProfileSource` trait: `name`, `index`, `trust`, `root`, `get_recipe`, `get_profile`
- Implementations: `DirSource`, `LayeredSource` (later-wins); git via CLI cache as
  `DirSource` after pin; HTTP stub only
- Config:
  ```toml
  [registries.official]
  git = "…"
  ref = "v1"
  # optional: trust = "official"

  [registries.scratch]
  path = "~/…"
  ```
- Cache: `~/.cache/isol8/registries/<name>/<pin>/` (or `XDG_CACHE_HOME`)
- Offline by default; `@registry list|update|install|show|verify`
- `isol8.lock` — registry pins + per-entry sha256; drift checked by `verify`
- Diff on install (added/changed/removed; sensitive paths, rw on real home,
  forbidden/ceiling flags); `--strict` hard-fails on forbidden/ceiling
- Capability ceiling metadata in manifest (`max_grant_outside_home`,
  `rw_outside_home_allowed`) — **install-time only**, not resolve-time enforcement
- Bare recipe ids still work; source label `registry:<trust>:<name>:<id>`

### Out of scope (still deferred)

- Signing (minisign/sigstore)
- Async/tokio registry stack — sync only (`git` CLI + filesystem)
- HTTP registries (error message only)
- Full ceiling enforcement at resolve time
- Optional `registry` cargo feature / crate split (Phase 9)

### Tests

- Unit: DirSource, LayeredSource, lockfile, trust parse, install diff flags
- Fixture: `tests/fixtures/registry/` (sample + sample-cache recipes)

### Docs checklist

- [x] New `_docs/registry.md`
- [x] `project-structure.md` — `registry.rs` + offline recipe load note
- [x] Trust model in AGENTS / instructions / recipes
- [x] This plan

### Gate

```sh
just ci
```

### Resume notes

**Completed 2026-07-24.** Phase 8 (wizard) also complete; next is Phase 9 (crate split).

#### Files

| Path | Role |
|------|------|
| `src/registry.rs` | Trust, DirSource, LayeredSource, RegistrySpec, lockfile, cache, open_offline, update, install diff, discover_offline_recipe_dirs |
| `src/cli/mod.rs` | `@registry list\|update\|install\|show\|verify`, RegistryArgs (`--lockfile`, `--no-lock`, `--strict`) |
| `src/cli/config.rs` | `[registries.*]` strip/parse into `Config.registries` |
| `src/recipe.rs` | `RecipeRegistry::load` pulls offline registry recipe dirs; skips unparseable registry TOML |
| `src/detect.rs` | `commands_trusted` allows `registry:official:…` / `registry:local:…` |
| `src/lib.rs` | Re-exports registry surface |
| `tests/fixtures/registry/` | Minimal path registry fixture |

#### Behaviour locked

1. **Offline by default** — no network on ordinary runs or `RecipeRegistry::load`.
2. **Git only via explicit update/install** when cache missing; pin = commit SHA.
3. **Path registries** open in place; pin = index content hash.
4. **HTTP** config parse ok; open/update error with “not implemented yet”.
5. **Trust defaults:** path→local, git→community, http→untrusted; config/manifest may override.
6. **commands_allowed:** official + local only (community/untrusted block detect.version / verify.cmd).
7. **Install diff** flags sensitive paths, rw on `#HOME`, forbidden, ceiling; `--strict` fails on FORBIDDEN/ceiling only.
8. **Lockfile discovery:** `./isol8.lock` if present; else `./isol8.lock` when
   cwd has `isol8.toml`/yaml; else `~/.config/isol8/isol8.lock`.

#### API surface (engine)

- `TrustLevel`, `RegistrySpec`, `DirSource`, `LayeredSource`, `Lockfile`, `ProfileSource`
- `open_offline`, `update_registry`, `diff_index`, `discover_offline_recipe_dirs`
- Recipe source label: `registry:<trust>:<name>:<id>`

#### Follow-ups

- HTTP registry + signing when needed
- Resolve-time capability ceiling (install-only today)
- Optional feature gate / crate split (Phase 9)
- `EmbeddedSource` as a `ProfileSource` wrapper (builtins stay in
  `RecipeRegistry` / `LayerRegistry` today — KISS; evo-repo §7.5 full trait
  surface can wrap them later if embedders need one stack)
- Separate `GitSource` type (today: `update_registry` → cache → `DirSource`)

---

## Phase 8 — Wizard

**Status:** **done** (2026-07-24)  
**Depends on:** Phase 4 + Phase 7  
**evo-repo:** §3.3, §6, §10 step 8  
**Goal:** First-run good path; managed sections safe to re-run.

### In scope

- `@cage new` / `@cage edit` interactive (dialoguer — **not** ratatui)
- Flow: always print `@cage detect` table first → home mode → toolchains →
  project dirs → preview → apply (+ optional `--verify`)
- Non-interactive: `--yes` (or `--preview`); bare `--tools` uses recipe defaults
- `toml_edit` surgery; `# isol8:managed` markers on toolchain sections
- Drift protection via managed-section hash in `~/.config/isol8/state.toml`
- Bundles: `--from bundles/…|path.toml` (offline registry index or filesystem)
- Security notes: `preview_security_notes` flags **rw** grants on `#HOME`
- Project-local write via `--path DIR`

### Out of scope (still open)

- Full TUI
- `@cage clone`, `@cage fix`
- Auto network fetch without prior `@registry update`
- Phase 9 crate split

### Open decisions (locked)

1. **Strategy defaults:** recipe `default_strategy` first (if defined), else
   heuristics (caches → `share`, version managers → `link`, else prefer
   link > share > isolate among defined strategies). See
   `wizard::default_strategy_for`.
2. **`inherit` + toolchains:** still written; strategy grants apply;
   materialization under `~` hits the **real** home (warning emitted).
3. **Project-local cages:** supported via `--path`; absolute `home` paths get a
   **portability warning only** (not an error).

### Tests

- Unit: `src/wizard.rs` (normalize_home, parse_tools_list, default_strategy_for,
  managed hash/drift, render/apply rewrite preserving `[[dirs]]`)
- Integration: `tests/wizard.rs` (non-interactive authoring, force/drift)

### Docs checklist

- [x] `instructions.md` — wizard walkthrough
- [x] `project-structure.md` — `wizard.rs` in layout
- [x] `AGENTS.md` — status, module, roadmap
- [x] `recipes.md` — strategy defaults used by wizard
- [x] This plan — status + resume notes + open decisions

### Gate

```sh
just ci
```

### Resume notes

**Completed 2026-07-24.** Restart from Phase 9 (crate split) when ready.

#### Files added/changed

| Path | Role |
|------|------|
| `src/wizard.rs` | Engine: `WizardRequest` / `WizardResult` / `DriftStatus`, normalize_home, parse_tools_list, default_strategy_for, tools_from_detect, expand_bundle / parse_bundle, managed hash, render/apply, state.toml |
| `src/cli/mod.rs` | `@cage new` / `edit` flags, detect table first, interactive (dialoguer) vs `--yes` / `--preview`, security notes, optional `--verify` |
| `src/lib.rs` | `pub mod wizard` |
| `tests/wizard.rs` | Integration: non-interactive write, drift / `--force` |
| `Cargo.toml` | `toml_edit` (always); `dialoguer` under `cli` feature |
| Docs | instructions, AGENTS, project-structure, recipes, this plan |

#### Behaviour locked

1. **Detect first** — every `@cage new`/`edit` prints the detect table (even with `--yes`).
2. **Interactive** — TTY + not `--yes` → dialoguer prompts; non-TTY requires `--yes` or `--preview`.
3. **Managed sections** — `[toolchains.*]` marked `# isol8:managed`; wizard rewrites them wholesale on edit; `[[dirs]]` and unknown keys preserved via `toml_edit`.
4. **Drift** — hash of managed toolchain content stored in `~/.config/isol8/state.toml`; hand-edited toolchains refuse rewrite without `--force` (also when state is missing for an existing file on edit).
5. **`managed` home** — `--home managed` → `home = "@managed/<cage_name>"`.
6. **Bundles** — offline only (`kind = "bundle"` TOML from registry cache or path); no auto fetch.
7. **Security preview** — prints notes for recipe strategies that grant **rw** on `#HOME` before write.
8. **CLI surface:**

```
isol8 @cage new <NAME> [--yes] [--home inherit|ephemeral|managed|PATH]
  [--tools nvm,cargo:share] [--dir PATH]... [--from bundles/…|path.toml]
  [--force] [--preview] [--verify] [--path DIR] [--profiles a,b]
isol8 @cage edit <NAME> [same flags]
```

#### Follow-ups

- Full TUI, `@cage clone`, `@cage fix` (not planned for Phase 9)
- Auto registry fetch (explicit `@registry update` remains the gate)
- Phase 9 crate split (`isol8-core` / `-registry` / `-cli`)

---

## Phase 9 — Crate split

**Status:** **done** (2026-07-24)  
**Depends on:** Phase 7–8 (boundaries proven)  
**evo-repo:** §7.1, §10 step 9  
**Goal:** Clean API seams without behavior change.

### Target layout (as shipped)

```
crates/isol8-core/     # engine: profiles, resolve, home, backends, detect, analyze,
                       #         recipe, cage, plan, context, env, filter, error, sandbox
crates/isol8-registry/ # offline registries: ProfileSource, DirSource, lockfile, cache,
                       #         trust (depends on isol8-core only)
crates/isol8-cli/      # CLI library: cli/*, wizard (depends on core + registry)
.                      # package name `isol8` — facade re-exporting all of the above;
                       # binary shim `src/bin_shim.rs`
```

### Rules (locked)

- CLI contains **no** policy logic — only prompts, rendering, config, meta-commands.
- Core does **not** depend on registry; discovery is wired via
  `recipe::set_offline_registry_provider` (facade/`ensure_registry_provider`).
- Public API stability: `use isol8::…` still works (modules + types re-exported).
- Feature gates on the root `isol8` package:
  - `default = ["cli", "registry"]`
  - `registry` → dep `isol8-registry`
  - `cli` → `registry` + dep `isol8-cli`
  - `field-test` → field-test bin
- Versions lockstep **0.2.6** (workspace).
- `profiles/` and `recipes/` stay at workspace root; `isol8-core` `build.rs` embeds them.
- Behavior unchanged for users of the `isol8` binary and `isol8` crate.

### Tests

- Entire previous suite green under `cargo test --workspace`
- Justfile gate uses `--workspace` for clippy/build/test

### Docs checklist

- [x] `project-structure.md` full rewrite of crate layout
- [x] `AGENTS.md` build/embed examples
- [x] README embed section
- [x] `_docs/instructions.md` embed section (workspace features)
- [x] This plan

### Gate

```sh
just ci
```

### Resume notes

**Completed 2026-07-24.** Evolution Phases 0–9 complete. Next is Phase 10
(Linux `--analyze` / shadow mode) — **deferred**; not started.

#### Files / layout

| Path | Role |
|------|------|
| `Cargo.toml` (root) | Workspace members; facade package `isol8`; features `cli` / `registry` / `field-test` |
| `src/lib.rs` | Facade: re-exports `isol8_core` + optional registry/cli; `ensure_registry_provider()` |
| `src/bin_shim.rs` | Binary: `ensure_registry_provider()` then `isol8_cli::cli::main()` |
| `crates/isol8-core/` | Engine crate (`isol8_core`); `build.rs` embeds `../../profiles` + `../../recipes` |
| `crates/isol8-registry/` | Offline registries (`isol8_registry`); depends on core only |
| `crates/isol8-cli/` | CLI + wizard (`isol8_cli`); field-test bin under `src/bin/` |
| `justfile` | `cargo test --workspace`, clippy/build `--workspace` |

#### Behaviour locked

1. **Facade API** — `isol8::Sandbox`, `isol8::profile::…`, `isol8::registry::…` (with features) unchanged for embedders.
2. **Engine-only** — `isol8 = { …, default-features = false }` pulls core only (no clap / registry / wizard).
3. **Registry provider** — core calls optional offline-dir hook; CLI binary installs it at startup; library users of config-backed registries call `isol8::ensure_registry_provider()` once.
4. **No policy in CLI** — merge/resolve/backends stay in core.
5. **Single binary UX** — `isol8` and `isol8-field-test` still built from the root package.

#### Public API impact

- Additive workspace crates; root `isol8` remains the recommended dependency.
- Direct path deps on `isol8-core` / `isol8-registry` / `isol8-cli` are possible for advanced split embeds.
- Feature `cli` now implies `registry` (was formerly a single crate feature set).

#### Known follow-ups (not blockers)

- Phase 10: Linux shadow observe
- HTTP registries, signing, full TUI / `@cage clone` / `@cage fix` (out of Phase 9)
- Optional publish of individual crates to crates.io (path/workspace today)

---

## Phase 10 — Linux `--analyze` (deferred)

**Status:** deferred  
**Depends on:** Phase 6 (suggestion UX proven); Phase 9 complete  
**evo-repo:** §8.5, §10 step 10  

Landlock emits no denial log on verified kernels (WSL2 5.15). Primary approach when
picked up: **shadow mode** (evaluate policy in userspace, opt-in permissive run),
not LD_PRELOAD. Do not start until Phases 5–6 prove the suggestion UX. Pick up when
Linux denial observation is prioritized over remaining registry/signing/TUI work.

### Resume notes

*(fill when started)*

---

## Cross-cutting work (every phase)

### Gate definition

A phase is **done** only when:

1. Code + tests merged on the working branch
2. `just ci` green (fmt, clippy `-D warnings`, build, test)
3. Field tests green if enforcement/materialization changed
4. Docs checklist items checked
5. This file’s phase status → **done** and resume notes filled
6. No silent widening of default grants

### Subagent usage (recommended)

| Work | Subagent |
|------|----------|
| Bulk fixture recipes, TOML ports | general-purpose / explore |
| Codebase search for integration points | explore |
| Implementation of a well-scoped phase slice | general-purpose (worktree if parallel) |
| Design ambiguity | ask user; do not invent |
| Final phase review | review skill only if user requests |

### Doc ownership map

| Doc | Update when |
|-----|-------------|
| `AGENTS.md` | status, modules, roadmap pointer |
| `_docs/project-structure.md` | modules, data flow |
| `_docs/profile-model.md` | schema / status table |
| `_docs/instructions.md` | user-facing CLI |
| `_docs/macos-support.md` / `linux-support.md` / `windows-support.md` | backend-specific analyze/materialize |
| `_docs/testing-strategies.md` | new field scenarios |
| `_docs/wip/multi-evo-plan.md` | **this file** — always |
| `_docs/inbox/evo-repo.md` | leave as design source; mark “superseded by plan” only if intentional |

### Backward compatibility

- All existing invocations without `--cage` keep current behavior
- Bare profile ids keep working after registry lands
- HOME default remains **real home** unless cage/profile/`--home` opts in

### Explicitly not in this evolution track

- Network tiers N1–N3 (roadmap Phase 3 of project-description)
- Seccomp / resource limits
- Windows AppContainer full enforcement (tracked in windows-review)
- Signing for registry artifacts

---

## Suggested first implementation PR (Phase 1 slice)

Minimal PR to unlock the rest:

1. `src/cage.rs` — load TOML, discover path, tests with temp dirs  
2. CLI `--cage`/`-c` + `ISOL8_CAGE` → fill Spec  
3. `@cage list` / `@cage show`  
4. Docs: instructions + plan resume notes  
5. `just ci`

No recipes, no materialization changes, no new dependencies.

---

## Changelog (plan document)

| Date | Change |
|------|--------|
| 2026-07-23 | Initial multiphase plan from evo-repo analysis; Phase 0 done |
| 2026-07-23 | Phase 1 done: `src/cage.rs`, CLI `--cage`/`@cage`, Spec.ephemeral_home |
| 2026-07-23 | Phase 2 done: Context, HomePlan plan/apply, @managed, dry-run home section; symlink field findings |
| 2026-07-24 | Phase 3 done: recipes + strategies, cage toolchains, builtin nvm/cargo/maven |
| 2026-07-24 | Phase 4 done: `@cage detect` / `verify`, `run_captured`, trust gate |
| 2026-07-24 | Phase 5 done: shared analyze layer, `--analyze`, NDJSON feed; Win hook deferred |
| 2026-07-24 | Phase 6 done: macOS log stream/show scrape, `--author` Seatbelt trace |
| 2026-07-24 | Phase 7 done: offline registries (`src/registry.rs`), `@registry`, lockfile, trust, install diff |
| 2026-07-24 | Phase 8 done: cage wizard (`src/wizard.rs`), `@cage new`/`edit`, managed sections, drift, bundles offline |
| 2026-07-27 | Recipe schema extended (post-Phase-3 follow-up): `tags`, `requires` (profile layers → layer stack), strategy `summary` / `danger` / `path_prepend`; later recipe source overrides an overlapping variant; registry files declaring `kind = "recipe"` warn instead of being skipped silently. Unblocks external registries authored against the target schema. |
| 2026-07-24 | Phase 9 done: Cargo workspace split (`isol8-core` / `isol8-registry` / `isol8-cli` + root facade); API-stable `use isol8::…`; registry provider hook; docs updated |
