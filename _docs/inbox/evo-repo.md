# isol8 — Cages, Recipe Registry & Policy Analysis

**Status:** design source of truth — **Phases 1–8 implemented** (see execution plan)  
**Execution plan:** [`_docs/wip/multi-evo-plan.md`](../wip/multi-evo-plan.md) (phased implementation, gates, resume notes)  
**Implemented (v0.2.6):** cages, Context/HomePlan, recipes/strategies, detect/verify,
shared + macOS `--analyze`, offline registries (`src/registry.rs`), cage wizard
(`src/wizard.rs`). **Still open on this track:** crate split (Phase 9), Linux
shadow `--analyze` (Phase 10). HTTP registries, signing, full TUI, `@cage clone`/`fix`
remain deferred design notes below.  
**Target:** post-0.2.x  
**Scope:** decouple policy evolution from the binary; make custom `$HOME` setup a one-command operation

---

## 1. Motivation

Today, running a tool under a replaced `$HOME` requires long manual tuning of path
grants. The recurring pain points:

- **Package caches** (`~/.m2`, npm cache, `~/.cargo/registry`, NuGet) must be
  relocated, shared, or linked — each needs a different treatment.
- **Version managers** (nvm, mise, sdkman, rustup) hardcode `$HOME`-relative paths
  and break under replacement unless the directory is materialized *and* the
  corresponding env var is repointed.
- Every user rediscovers the same fixes independently, and each fix is trapped in
  a local config file.

Two observations drive the design:

1. These are not purely *policy* problems. A path grant alone does not fix `nvm` —
   the directory must also exist in the replaced home (link, copy, or mkdir) and
   `NVM_DIR` must point at it. The unit of sharing is **grant + materialization +
   env**, not a grant list.

2. The knowledge is generic and the configuration is personal. That split suggests
   two layers: a **shared library** distributed out-of-band, and a **local
   selection** that names what to use.

---

## 2. Glossary

Terms are used precisely throughout this document. Where a word already has a
meaning in isol8 or in general usage, the divergence is noted.

### 2.1 The two layers

Everything below divides into a **shared library** distributed out-of-band, and a
**local selection** naming what to use from it.

| Term | Ownership | Location | Definition |
|---|---|---|---|
| **Cage** | local | `~/.config/isol8/cages/` or project | A named, switchable isolation unit: one home mode + a profile list + per-toolchain strategy choices. What the user selects with a single flag. |
| **Profile** | shared | registry / builtin | A raw policy layer — `filter`, `requires`, `paths`, `[[policies]]`. Exists today; unchanged by this proposal. |
| **Recipe** | shared | registry | A profile extended with toolchain integration: `detect`, `[strategies.*]`, `home` materialization, env vars, `verify`. The unit that makes one tool work under a replaced home. |
| **Bundle** | shared | registry | A curated set of recipes and profiles for a common setup, e.g. `bundles/polyglot-agent`. Skips the wizard for typical cases. |

### 2.2 Core vocabulary

**Cage** — chosen over `env`, `config`, `workspace`, and `sandbox` (§9.1). Used in
the Kubernetes-context sense: a named selection you switch between, not a
container or a directory. A cage names an isolation unit; it does not *contain*
anything. Reserved surface: `--cage`/`-c`, `ISOL8_CAGE`, `@cage *`, `[cage]`.

**Registry** — a source of profiles, recipes, and bundles. Concretely: a git repo,
an HTTP-served tree, or a local folder, all behind one `ProfileSource` trait.
Multiple registries compose with later-wins precedence. Not a server — an HTTP
registry needs no logic beyond static file hosting.

*Note:* "repository" is avoided as a term of art. A registry usually *is* a git
repository, but the word is overloaded (the isol8 source repo, `~/.m2/repository`)
so "registry" is used for the concept and "repo" only for actual git repos.

**Strategy** — one of three named ways to reconcile a tool's `$HOME`-relative
state with a replaced home. A recipe declares what each strategy means for its
tool; a cage picks which one to use. The three are `share` (symlink to the real
path, kept writable), `link` (symlink, read-only with writable overlays on mutable
subpaths), and `isolate` (fresh directory in the replaced home). Full definitions
and per-tool examples in §4.1.

A strategy is **not** a single grant — see *materialization*.

**Materialization** — creating the filesystem state a replaced home needs: links,
directories, seeded files. Expressed as the `home` primitive, alongside `paths` and
env vars. The distinction that motivates this proposal: a path grant alone does not
make `nvm` work; the directory must also *exist* in the replaced home and
`NVM_DIR` must point at it. Grant + materialization + env is the atomic unit.

**Selector** — the `filter` expression declaring which platforms (and optionally
arch) a profile or recipe applies to. Authoritative: the resolver matches on the
selector, never on the filename. Selectors across variants of one id must be
disjoint (§5.4).

**Variant** — one of several files sharing an `id` but carrying disjoint
selectors, e.g. `toolchains/nvm.toml` (macOS + Linux) and
`toolchains/nvm.windows.toml`. The filename suffix is descriptive convention only.
Cages name the bare `id` on every platform.

### 2.3 Homes and paths

**Replaced home** — the substituted `$HOME` under which the confined process runs.
Preferred over "fake home" or "custom home" for consistency with the existing
`--home` flag and HOME-replacement docs.

**Real home** — the user's actual `$HOME`, reachable from a policy via the
existing `#HOME` token even under replacement.

**Home mode** — a cage's choice of `inherit` (keep the real home, current
default), `@managed/<id>` (isol8-managed directory under the platform data dir), or
`ephemeral` (fresh tmpdir per run).

**Managed root** — the base directory holding `@managed/*` homes. Part of
`Context`, so it is injectable rather than read from the environment implicitly.

**Token** — a path fragment resolved against ambient state rather than used
literally: `~`, `#HOME`, `@managed/<id>`. Resolution requires a `Context` (§7.4);
this is why paths in cage and recipe files are tokens, not paths.

### 2.4 Tooling and process

**Managed section** — a TOML table the wizard owns and rewrites wholesale, marked
`# isol8:managed`. Contrasted with *merged* (entries added/removed, order
preserved) and *user* (never touched). Content-hashed to detect hand edits (§3.3).

**Detection** — read-only probing of the real home to discover which toolchains
exist, via each recipe's `detect.probe`. No side effects; a separate command from
setup.

**Verification** — running each recipe's `verify.cmd` inside the cage to prove it
works, rather than asserting it. Registry-extensible: a new recipe brings its own
smoke test.

**Analysis** (`--analyze`) — observing denials during a confined run and mapping
them to recipes that would allow them. Diagnoses what verification reports as
broken.

**Denial** — a single access refused by the enforcement backend, normalized across
platforms into `{ path, access, count, pid, exe }`. Collapsed to roots before being
matched against the registry index.

**Plan / apply** — every mutating operation computes a `Plan` with no side effects,
which is then applied separately. Makes dry-run, wizard preview, and verification
the same code path, and gives embedders something to render.

---

## 3. Cages

### 3.1 File format

```toml
schema = 1
name = "work"
home = "@managed/work"          # @managed/<id> | inherit | ephemeral

profiles = ["base", "toolchains/node", "toolchains/rust", "agents/claude-code"]

# isol8:managed — regenerated by `isol8 @cage edit`; edits here are lost
[toolchains.nvm]
strategy = "link"

# isol8:managed
[toolchains.maven]
strategy = "share"

# user-owned below this point
[[dirs]]
path = "~/work/acme"
access = "rw"
```

`home` values:

- `inherit` — keep the real `$HOME` (current default behaviour)
- `@managed/<id>` — isol8-managed directory under the platform data dir
- `ephemeral` — fresh tmpdir per run, discarded on exit

Absolute paths are permitted but flagged by lint as non-portable.

### 3.2 Resolution order

```
ISOL8_CAGE                              (env var)
  → --cage / -c <name>                  (explicit flag)
  → ./isol8.toml  [cage] section        (project-local, checked in)
  → ./.isol8/cage.toml
  → walk up to git root, retry the two above
  → ~/.config/isol8/cages/default.toml
```

Usage collapses to one knob:

```
isol8 -c work claude          # explicit
isol8 claude                  # cage named by ./isol8.toml
```

Existing flags (`--home`, `--add-dirs-rw`, `--profile`) remain and override the
cage. No breaking change to current invocations.

### 3.3 Managed sections and hand edits

The wizard **owns whole sections**; the user owns everything else.

| Ownership | Applies to | Wizard behaviour |
|---|---|---|
| Managed | `[toolchains.*]` | Rewritten wholesale on each run |
| Merged | `profiles` list | Entries added/removed, order preserved |
| User | `[[dirs]]`, comments, unknown keys | Never touched |

Implementation via `toml_edit`: build the replacement table detached, then assign
into the slot. Comments *above* a managed table survive; comments *inside* it do
not, which is correct since the wizard authored the contents.

**Drift protection.** A content hash of each managed section is stored in
`~/.config/isol8/state.toml`. If a section's current hash does not match what the
wizard last wrote, the wizard stops and asks rather than silently discarding hand
edits. This is what makes the wizard safe to re-run.

---

## 4. Recipes

A recipe packages everything needed to make one toolchain work under a replaced
home.

```toml
schema = 1
id = "toolchains/nvm"
kind = "recipe"
filter = { os = ["macos", "linux"] }     # authoritative platform selector
summary = "Node Version Manager"

[detect]
probe = { path = "~/.nvm" }
version = "nvm --version"

[verify]
cmd = "node --version"
expect = "^v\\d+"

[strategies.link]
home = [{ kind = "link", from = "#HOME/.nvm", to = "~/.nvm" }]
paths = [
  { path = "#HOME/.nvm/versions", access = "ro" },
  { path = "#HOME/.nvm/alias",    access = "rw" },
]
env = { NVM_DIR = "~/.nvm" }

[strategies.isolate]
home = [{ kind = "mkdir", path = "~/.nvm" }]
paths = [{ path = "~/.nvm", access = "rw" }]
env = { NVM_DIR = "~/.nvm" }
```

### 4.1 Strategies

| Strategy | Mechanism | Typical use |
|---|---|---|
| `share` | symlink replaced→real, `rw` on the real path | Caches you want warm: `~/.m2/repository`, npm cache, cargo registry |
| `link` | symlink replaced→real, `ro` + writable overlay for mutable subpaths | Version managers: read installed toolchains, forbid mutation |
| `isolate` | fresh dir in the replaced home, optionally seeded | Config the agent may rewrite: `~/.claude`, `~/.config/gh` |

Version managers are **not uniform inside** — `~/.nvm/versions` should be `ro`
while `~/.nvm/alias` is `rw`; `~/.cargo/registry` and `bin` are `ro` while
`.package-cache` is `rw`. A strategy is therefore a per-tool recipe, not a single
grant. That per-tool knowledge lives in the registry, not the binary.

### 4.2 Home materialization

New primitive alongside `paths` and `env`:

```toml
home = [
  { kind = "link",    from = "#HOME/.nvm", to = "~/.nvm" },
  { kind = "mkdir",   path = "#HOME/.cache/foo" },
  { kind = "seed-ro", from = "~/.gitconfig", to = "#HOME/.gitconfig" },
  { kind = "copy",    from = "~/.npmrc", to = "#HOME/.npmrc" },
]
```

Materialization must be **idempotent** and expressed as plan/apply (§7.2) so the
wizard can preview it and `--analyze` can diff it.

### 4.3 Registry kinds

The registry holds three species, distinguished by `kind`:

- `profile` — raw policy layer (what exists today)
- `recipe` — toolchain integration, as above
- `bundle` — curated template, e.g. `bundles/polyglot-agent`

Bundles make the first run good: `isol8 @cage new work --from official:bundles/polyglot`
skips the wizard entirely for common setups.

---

## 5. Registry

### 5.1 Sources

Configured in `isol8.toml`, composed with existing later-wins precedence:

```toml
[registries]
official = { git = "https://github.com/mdnmdn/isol8-recipes", ref = "v1" }
work     = { git = "git@github.com:acme/isol8-recipes", ref = "main" }
scratch  = { path = "~/dev/my-recipes" }
```

Reference syntax `registry:namespace/id@version`. Bare ids continue to work, so
existing configs are unaffected.

Backends: git, plain HTTP (tarball + index), local folder. All three behind one
`ProfileSource` trait (§7.3), so an HTTP-only registry needs no server logic —
GitHub Pages, S3, or raw.githubusercontent are sufficient.

### 5.2 Layout

```
isol8-recipes/
├── registry.toml          # schema version, maintainers, trust metadata
├── index.json             # generated in CI
└── recipes/
    ├── toolchains/nvm.toml            # filter: macos, linux
    ├── toolchains/nvm.windows.toml    # filter: windows
    └── bundles/polyglot.toml
```

`index.json` entries:

```json
{ "id": "toolchains/nvm", "kind": "recipe", "os": ["macos", "linux"],
  "file": "recipes/toolchains/nvm.toml",
  "strategies": ["link", "isolate", "share"], "detects": "~/.nvm",
  "summary": "Node Version Manager" }
```

Search is then one small HTTP GET rather than a clone or an API call. The wizard's
toolchain list is `index.filter(kind == recipe && os matches && detect probe hits)` —
**no hardcoded tool knowledge in the binary**. Adding a toolchain is a PR to the
registry, not a release of isol8.

### 5.3 Caching

Cache to `~/.cache/isol8/registries/<name>/<commit>/`. **Offline by default**;
refresh only on explicit `isol8 @registry update`. A sandbox tool that silently
fetches policy over the network at exec time is a supply-chain hole in a security
product.

### 5.4 Platform selectors and file layout

**Every profile and recipe carries a platform selector**, single or multiple, using
the existing `filter` mechanism. The selector is authoritative — it is what the
resolver matches on. File naming is a readability convention layered on top, not a
second mechanism.

**Convention: one file per distinct selector.** Platforms sharing an
implementation share a file; platforms that diverge get their own with a suffix.

```toml
# recipes/toolchains/nvm.toml — macOS and Linux behave identically
filter = { os = ["macos", "linux"] }
id = "toolchains/nvm"
```

```toml
# recipes/toolchains/nvm.windows.toml — different paths, different policies
filter = { os = ["windows"] }
id = "toolchains/nvm"
```

The cage file names `toolchains/nvm` on every platform; the resolver selects among
candidates by `filter` match against the active `Context::platform` (§7.4). The
`.windows` suffix is descriptive — the resolver never parses it. That keeps
filename and behaviour from drifting apart, and means a variant can be split out of
a shared file later by narrowing two `filter` lines with no change to any cage.

**Rules:**

- Selectors across variants of one id must be **disjoint** — no platform may match
  two candidates. Ambiguity is an error, not a precedence question.
- Coverage need not be complete. A recipe existing only for macOS and Linux is
  valid; on Windows it simply does not resolve, and the wizard omits it from the
  toolchain list.
- Suffix vocabulary matches the selector values (`.windows`, `.macos`, `.linux`).
  A file with a multi-platform selector takes no suffix.

**Registry CI lint (required):** variants of the same id must declare the same
`[strategies.*]` names and the same `verify` command, and their selectors must not
overlap. Internals may differ completely; the interface must not. This is the
invariant that prevents silent divergence — without it, `nvm.windows.toml` grows a
strategy the macOS file lacks and cages stop being portable.

Cross-platform resolution for this lint is why `Context` is injectable (§7.4):
CI resolves a Windows cage on a Linux runner.

### 5.5 Trust

A recipe specifies filesystem mutations (symlinks into the real home) and commands
to execute (`detect.version`, `verify.cmd`). That is a materially larger blast
radius than a path grant.

1. **Lockfile.** `isol8.lock` pins commit SHA + content hash per recipe. Drift is
   an error, not a silent update.
2. **Diff on install.** `@registry install` prints the effective delta before
   writing; new `rw` grants highlighted; anything touching `~/.ssh`, `~/.aws`,
   `~/.gnupg`, or `#HOME` flagged prominently.
3. **Capability ceiling.** Config-level `max_grant` that a registry recipe cannot
   exceed regardless of what it declares. Non-official registries default lower.
4. **Command execution gate.** `detect.version` and `verify.cmd` from non-official
   sources require explicit trust — or run inside isol8 itself, which is fitting
   and likely cheap.

Signing (minisign / sigstore) is deferred; the lockfile covers most of the risk.

---

## 6. Wizard

Detection and setup are **separate commands**, and detection has no side effects.

### 6.1 Commands

```
isol8 @cage list
isol8 @cage show work
isol8 @cage detect                     # read-only probe, no writes
isol8 @cage new work                   # interactive
isol8 @cage new work --from official:bundles/polyglot
isol8 @cage edit work
isol8 @cage clone work work-experiment
isol8 @cage verify work
isol8 @cage fix work --grant ~/.m2/wrapper:rw
```

Non-interactive form for CI and dotfiles:

```
isol8 @cage new work --home managed --tools nvm,cargo,maven --yes
```

### 6.2 Detection

Runs first, always, and prints before any prompt:

```
Detected in ~:
  ✓ nvm      ~/.nvm              (node 20.11, 22.3)
  ✓ cargo    ~/.cargo, ~/.rustup
  ✓ maven    ~/.m2               (repository 4.2 GB)
  ✓ docker   ~/.docker           (context: desktop-linux)
  · sdkman   not found
```

Detection is `stat` on `detect.probe` plus an optional version command. Most of the
perceived quality of the wizard lives here — a tool that already knows what you
have feels categorically different from one that interrogates you.

### 6.3 Flow

1. **Home** — `inherit` / `@managed` / `ephemeral`, one line of explanation each.
   Everything else hangs off this choice.
2. **Toolchains** — multi-select, pre-checked from detection. Strategy defaults per
   tool (version managers → `link`, caches → `share`, agent config → `isolate`).
   Show the default; allow override; do not force a per-tool decision.
3. **Project dirs** — offer `$PWD` and the git root.
4. **Preview** — generated TOML *and* effective policy, reusing `--show-policies`.
   Every `rw` grant reaching outside the replaced home is highlighted. That is the
   security-relevant surface and it must be impossible to miss.
5. **Materialize + verify** — create the home, make links, then run verification.

### 6.4 Implementation

`inquire` or `dialoguer`, sequential prompts. **Not `ratatui`** — a full TUI is a
meaningful dependency and a second UI to maintain, for perhaps 20% more polish.
Revisit only if the flat flow proves limiting.

### 6.5 Verification

The final step proves the cage works rather than asserting it:

```
$ isol8 @cage verify work
  ✓ home materialized     ~/.local/share/isol8/homes/work
  ✓ nvm      node --version → v22.3.0
  ✓ cargo    cargo --version → 1.84.0
  ✗ maven    mvn --version → EACCES ~/.m2/wrapper
             fix: isol8 @cage fix work --grant ~/.m2/wrapper:rw
```

Each recipe declares its own `verify.cmd`, so verification is registry-extensible
at no additional cost. Failures hand off to `--analyze` (§8).

---

## 7. Library split

### 7.1 Crates

```
isol8-core        schema types, merge rules, policy resolution, home planning
isol8-registry    sources, cache, index, lockfile, trust
isol8-cli         prompts, rendering, toml_edit surgery, meta-commands
```

Start with three. Splitting a workspace later is easy; unsplitting is painful, and
every boundary is an API to maintain. Candidate future seams: `isol8-detect`
(probes + denial normalization), `isol8-sandbox` (platform enforcement),
`isol8-schema` (types alone, for tooling).

**Rule:** the CLI contains no policy logic. If `isol8-cli` ever decides *what* a
strategy means rather than rendering it, the boundary has leaked.

**Async:** keep `isol8-registry` synchronous (`ureq`, `git2`) or feature-gate an
async surface. A sandbox launcher pulling in tokio indicates bad layering.

### 7.2 Plan / apply

Every mutating operation exposes `plan()` separately from `apply(plan)`. This makes
dry-run, wizard preview, and `verify` the same code path, and gives embedders
something to render in their own UI.

```rust
let cage = Cage::resolve("work")?;
let policy = Policy::resolve(&cage, &registry, &ctx)?;
let plan = HomePlan::compute(&policy, &ctx)?;   // no side effects
println!("{}", plan.render());
plan.apply()?;
```

### 7.3 Ingress: file, path, or struct

Three entry points, one pipeline. Everything downstream accepts only `Env`.

```rust
let cage = Cage::resolve("work")?;              // isol8 does discovery
let cage = Cage::from_path("./team.toml")?;     // host picked the file
let cage = Cage::builder()                      // host built it directly
    .home(Home::Managed("work".into()))
    .profile("toolchains/nvm")
    .dir("~/work/acme", Access::Rw)
    .build()?;
```

**Typestate for validation.** Struct injection risks a host constructing a
syntactically valid but semantically broken cage — unknown recipe, nonexistent
strategy, contradictory grants — discovered only at enforcement time.

```rust
pub struct Cage<S = Validated> { /* … */ }
pub struct Raw;
pub struct Validated;

impl Cage<Raw> {
    pub fn validate(self, reg: &dyn ProfileSource)
        -> Result<Cage<Validated>, Vec<Diagnostic>>;
}

impl Policy {
    pub fn resolve(cage: &Cage<Validated>, reg: &dyn ProfileSource, ctx: &Context)
        -> Result<Policy>;
}
```

`Policy::resolve` accepts only `Cage<Validated>`, so skipping validation is
unrepresentable. `builder().build()` returns `Cage<Raw>` — cheap, no registry
needed. `Vec<Diagnostic>` rather than a single error, because an editor extension
wants every problem with spans.

### 7.4 Ambient context is explicit

`~/.nvm`, `#HOME`, `@managed/work` are tokens, not paths. Resolving them implicitly
would mean reading environment variables behind the host's back.

```rust
pub struct Context {
    pub real_home: PathBuf,
    pub cwd: PathBuf,
    pub platform: Platform,
    pub managed_root: PathBuf,
}

impl Context {
    pub fn from_environment() -> Result<Self>;   // the CLI calls this
}
```

This also enables hermetic tests and cross-platform resolution — resolving a
Windows cage on Linux for the CI lint of §5.4.

### 7.5 Registry as a trait

```rust
pub trait ProfileSource {
    fn get(&self, id: &ProfileId) -> Result<Option<Profile>>;
    fn index(&self) -> Result<&Index>;
    fn trust(&self) -> TrustLevel;
}
```

Ship `EmbeddedSource` (the ~70 builtins), `DirSource`, `GitSource`, `HttpSource`,
and `LayeredSource` composing them with existing precedence. The CLI assembles the
standard stack; an embedder assembles anything. `trust()` is where §5.5 command
gating hooks in.

### 7.6 Feature gates

```toml
[features]
default = ["toml-config"]
toml-config = ["serde", "toml_edit"]
registry    = ["ureq", "git2"]
```

`default-features = false` yields the schema and policy engine with no I/O
dependencies — useful for a host with vendored recipes and its own config reading.

### 7.7 Formatting state stays out of the core

`CageDocument` wraps `toml_edit::Document` and *produces* an `Env`. The wizard talks
to `CageDocument`; everything else talks to `Env`.

```rust
let mut doc = CageDocument::open("~/.config/isol8/cages/work.toml")?;
doc.set_managed_section("toolchains.nvm", nvm_config)?;   // hash-checked
doc.save()?;
let cage = doc.to_cage()?;
```

Struct injection bypasses the file, so there is no managed-section marker and no
round-trip — correct, and the reason format preservation must not live in `Env`.

---

## 8. `--analyze` (policy diagnosis)

### 8.1 Purpose

Answer one question: *what did the sandbox deny, and which recipe would allow it?*
Turns the manual tuning loop into a suggestion.

Exposed as a flag rather than a meta-command, since it wraps a normal run:

```
isol8 --analyze claude
isol8 --analyze --author claude       # emit a draft recipe
```

**Scope decision: macOS and Windows only in the first implementation.** Linux is
deferred — see §8.5.

### 8.2 macOS (Seatbelt)

Seatbelt logs every denial. Two modes:

**Default — log scraping.** Denials appear in the unified log under the `Sandbox`
subsystem:

```
log stream --style ndjson --predicate 'sender == "Sandbox" AND eventMessage CONTAINS "deny"'
```

Spawn concurrently with the confined process, filter by child PID, parse
`deny(1) file-read-data /Users/x/.nvm/…`. Requires no policy change. Watch the
startup race: begin the stream, wait for the first heartbeat, then exec — otherwise
fast-failing commands produce nothing.

**`--author` — trace mode.** Regenerate the sbpl with `(trace "/tmp/isol8-analyze.sbpl")`.
Seatbelt writes a ready-made allow profile of everything actually touched, in
exactly the shape needed for recipe authoring. Trace implies **permissive
execution**, so it must be explicitly opt-in and never a default.

### 8.3 Windows (hook DLL)

`isol8-winhook.dll` already intercepts path operations to enforce. The denial
already has a decision point:

```c
if (!policy_allows(path, access)) {
    if (analyze_mode) log_denial(path, access, caller_module);
    return STATUS_ACCESS_DENIED;
}
```

Write NDJSON to a named pipe or per-run file, read by the parent. Richer data than
either Unix platform: the calling module attributes a denial to `node.exe` vs a
child `pnpm.exe`.

**Caveat to surface in output:** user-mode hooks miss anything bypassing the hooked
entry points. Report *observed* denials and say so, rather than implying
exhaustiveness.

### 8.4 Shared analysis layer

Both backends normalize into one record, so the interesting logic is written once:

```rust
pub struct Denial {
    pub path: PathBuf,
    pub access: Access,        // read | write | exec | metadata
    pub count: u32,
    pub pid: u32,
    pub exe: Option<String>,
}
```

Post-processing, platform-independent:

1. **Collapse to roots.** 400 denials under `~/.m2/repository/…` → one `~/.m2`
   entry. Walk up while the prefix is stable; stop at home-dir children.
2. **Match the registry index.** Each recipe publishes the path prefixes it grants;
   a denial root matching `toolchains/maven` becomes a suggestion.
3. **Classify home-materialization cases.** A denial on `#HOME/.nvm` under a
   replaced home may mean the path *does not exist there*, not that it is
   ungranted. Different fix — a `home` link, not a `paths` entry. Detect by
   stat-ing the real home for the same relative path. **This is the case that
   catches the nvm/mise scenarios.**
4. **Emit** a suggestion list, or with `--author`, a draft recipe.

```
$ isol8 --analyze claude
Observed 1,204 denials (macOS unified log)

  ~/.m2/repository        847 r    → official:toolchains/maven
  ~/.nvm                  201 rx   → official:toolchains/nvm   [needs home link]
  ~/.local/share/mise      93 rw   → official:toolchains/mise  [needs home link]
  ~/.config/gh             63 r    → no match; add manually?

  isol8 @cage fix work --add toolchains/maven,toolchains/nvm,toolchains/mise
```

### 8.5 Linux — deferred, and why

Landlock emits **nothing** on denial; violations return `EACCES`/`EPERM` to the
process and the kernel logs no record. Audit support exists (6.15+,
`LANDLOCK_RESTRICT_SELF_LOG_*`, `AUDIT_LANDLOCK_*`) but requires auditd, root, and
a kernel far newer than the WSL2 5.15 currently verified. Treat it as an
opportunistic fast path, never the mechanism.

Options when Linux is picked up:

| Approach | Cost | Coverage |
|---|---|---|
| `LD_PRELOAD` shim on `open`/`openat`/`stat` | trivial | Misses static binaries and Go programs (much of the relevant tooling) |
| `ptrace(PTRACE_SYSCALL)` / seccomp-unotify | substantial | Complete; gives path *and* access mode from `O_RDWR` vs `O_RDONLY` |
| Shadow mode — no ruleset applied, evaluate would-be policy in userspace | substantial | Complete, including paths that would abort the process early |

**Shadow mode should be the primary Linux implementation, not a fallback.** A
denial-driven analyzer only observes denials the process *survives*. If `nvm` dies
on its first failed read, one path is reported when the real answer is a dozen —
forcing an iterate-until-quiet loop. Shadow mode gives the complete trace on the
first run, at the cost of a permissive execution that must be opt-in.

---

## 9. Open points

### 9.1 Naming (resolved)

**`cage`** — the named isolation unit. Chosen over `env` (collides with
environment variables, which isol8 manipulates), `config` (collides with
`isol8.toml`; `-c work` would read as "which config file?"), `workspace` (implies
a directory), and `sandbox` (generic — every profile is also a sandbox).

Reserved surface: `--cage` / `-c`, `ISOL8_CAGE`, `@cage *`,
`~/.config/isol8/cages/`, `[cage]` in `isol8.toml`.

### 9.2 Portability of cages

If cages are checked into project repos, they cross machines. That argues for zero
absolute paths — `home = "@managed/work"` resolving per-platform, never
`/Users/marco/…`. Enforceable by lint. Deferred: decide whether project-local cages
are a supported use case or an accident.

### 9.3 Undecided

- **Strategy defaults** — should a recipe declare `default_strategy`, or does the
  wizard decide by category? Recipe-declared is more flexible; wizard-decided is
  more consistent.
- **`inherit` + toolchains** — do strategies mean anything when the home is not
  replaced? Probably a no-op, but `share` grants may still be wanted.
- **Ephemeral home lifecycle** — cleanup on exit, on next run, or never? Interacts
  with warm caches.
- **Recipe versioning** — is `@^1.2` per-recipe, or does a registry version as a
  unit? Per-recipe is more precise, more lockfile churn.
- **Selector granularity** — is `os` sufficient, or do variants need arch
  (`arm64` vs `x86_64`) and libc (glibc vs musl)? `filter` already supports arch;
  the question is whether the file-suffix convention should extend to it before it
  becomes a naming problem.
- **Symlink semantics under enforcement** — Seatbelt, Landlock, and the Windows
  hook resolve links differently. Whether a grant on a link target suffices needs
  verification per platform before `link` strategies are trusted.
- **Windows and symlinks** — `link` strategies need Developer Mode or elevation.
  Junctions are a partial workaround for directories; decide the fallback.
- **First embedder** — the library API should be shaped by one honest consumer.
  If that is `--analyze` itself, design for it and let the public API follow.

---

## 10. Sequencing

Each step is independently useful and shippable.

| # | Deliverable | Depends on | Rationale |
|---|---|---|---|
| 1 | `@cage` with hand-written TOML, no wizard | — | Delivers the one-knob invocation immediately |
| 2 | `home` materialization primitive | 1 | Makes version managers work at all |
| 3 | `[strategies.*]` in profiles | 2 | Makes toolchains declarative |
| 4 | Detection + `@cage verify` | 3 | Useful from the CLI alone; feeds the wizard |
| 5 | `--analyze` on Windows | 4 | Hook already has the decision point — cheapest way to prove the normalization layer and suggestion engine |
| 6 | `--analyze` on macOS | 5 | Log scraping against a proven analysis layer |
| 7 | Registry (git/http/local + index + lockfile) | 3 | Contents informed by what steps 4–6 reveal users actually need |
| 8 | Wizard | 4, 7 | Front-end over working machinery — a weekend, not a project |
| 9 | Crate split | 7 | Boundaries proven under real use rather than guessed |
| 10 | `--analyze` on Linux (shadow mode) | 6 | Largest implementation cost; defer until the payoff is demonstrated |

Two notes on ordering:

**Registry after detection, not before.** Step 4's detection output reveals which
recipes are actually needed, which beats guessing initial registry contents. The
registry then distributes something already proven rather than opening an empty
shelf.

**Windows before macOS for `--analyze`.** Counterintuitive given macOS is the
primary target, but the hook DLL already contains the denial decision point, making
it the cheapest place to validate that the shared analysis layer and suggestion
engine are correct.
