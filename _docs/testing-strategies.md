# isol8 — Testing Strategies

How `isol8` is verified. Two layers: **unit/integration tests** (fast, in-process,
`cargo test`) and **field tests** (a standalone client that spawns the *real*
sandbox against an ad-hoc home + profile and reports what was actually allowed or
denied). Field tests are the ground truth — a profile is only correct if the OS
enforces it.

> Status: implemented (macOS). Unit + integration tests (`cargo test`) and the
> field-test binary `src/bin/isol8-field-test.rs` (`just field-test`) are in place
> and green on macOS; scenarios 1–7 enforce, the network scenario (8) is `SKIP`
> until the net tiers land. The Linux path scenarios `SKIP` until that backend
> exists. See [`AGENTS.md`](../AGENTS.md).

---

## 1. Layers at a glance

| Layer | Where | What it proves | Runs on |
|-------|-------|----------------|---------|
| Unit | `src/**` `#[cfg(test)]` | Pure logic: profile merge, `requires` resolution, env allowlist, HOME-first resolution, filter matching, `--show-policies` rendering. | All platforms, no privileges. |
| Integration | `tests/*.rs` | Crate wired end-to-end *without* exec: load profiles → select layers → filter → resolve → merge → render. | All platforms. |
| Field | `src/bin/isol8-field-test.rs` | The OS actually enforces the policy: denied paths fail, granted paths work, env is sanitized, scratch HOME is in effect. | Per-OS, best-effort, prints a report. |

Unit and integration tests never touch the real filesystem outside a temp dir and
never require the backend to be functional. Field tests require a working backend
(Landlock on Linux, Seatbelt on macOS) and degrade gracefully where it is absent.

---

## 2. Unit & integration tests

Standard `cargo test`. Keep them deterministic and platform-independent:

- **Profile merge** — deny-first union, highest-layer-explicit-grant-wins, env
  defaults, network domain union. (`tests/profile_merge.rs`, `src/profile.rs`)
- **Inheritance** — `requires`/`extends` DFS: deps-first topo order, cycle
  detection, dedup, selection-order tiebreak. (`tests/profile_merge.rs`,
  `src/profile.rs`)
- **Env construction** — only the allowlist survives; HOME override applied first.
  (`src/env.rs`)
- **Path matchers** — `subpath` / `literal` / `prefix` / `regex` accept/reject.
- **Policy render** — a fixed profile stack renders to the expected effective
  policy (snapshot-style string compare). (`src/backends/macos.rs`)
- **Profile registry** — all embedded `profiles/**/*.toml` parse cleanly.
  (`src/profile.rs::all_builtin_profiles_parse`)
- **Profile-path overlay** — a `--profile-path` file or directory overrides
  same-named built-in layers and adds new ones. (`tests/profile_path.rs`)
- **Filters & auto-selection** — executable/OS/arch constraints, `[[policies]]`
  folding, and `auto_profiles` behaviour. (`src/filter.rs`, `tests/profile_filters.rs`)

These must pass on Linux, macOS, WSL2, and Windows alike — no real sandboxing
involved, so they are the portable backbone of CI.

### 2.1 Filter unit tests (`src/filter.rs`)

Filter logic is pure and tested in-process:

| Case | Expect |
|------|--------|
| `executable_basename` | `/usr/bin/claude` → `claude` (path and `.exe` stripped) |
| `filter_matches` AND semantics | All non-empty axes (`os`, `arch`, `executables`) must match |
| Executable match | Basename **or** full argv[0] literal (e.g. `/opt/bin/claude`) |
| `is_auto_selectable` | Only layers with `filter.executables` are auto-candidates |
| `apply_layer_filter` | OS/arch mismatch → empty paths/env/macos; `requires` kept |
| `apply_policies` | Matching `[[policies]]` entries fold into unconditional fields |

### 2.2 Filter integration tests (`tests/profile_filters.rs`)

These wire the public API (`select_layer_names`, `resolved_layers`,
`effective_policy`) against the embedded profile tree — no sandbox exec:

| Case | Command / setup | Expect |
|------|-----------------|--------|
| Auto-select by basename | `claude --version`, `auto_profiles=true` | `agents/claude-code` in layer stack |
| Auto-select by full path | `/usr/bin/claude`, `auto_profiles=true` | Same (basename extraction) |
| No false positive | `cargo build`, `auto_profiles=true` | `agents/claude-code` **not** selected |
| Auto off | `claude`, `auto_profiles=false` | Agent layer skipped unless `--profile` names it |
| Explicit override | `--profile agents/claude-code` + `cargo build` | Agent layer selected anyway |
| Grant folding | `claude` vs `cargo` with only auto-selected defaults | `~/.claude` grants present only for `claude` |
| Policy executable filter | `--profile-path` overlay with `[[policies]]` | Policy paths fold only when executable matches |
| OS layer filter | Explicit `linux/system-runtime` on macOS (or vice versa) | Paths cleared; `requires` deps still resolve |
| End-to-end | `resolve::effective_policy` for `claude` | Layer stack + merged grants include agent paths |

Default profile stacks in these tests use `base` plus the OS-appropriate
`macos/system-runtime` or `linux/system-runtime` layer so behaviour matches
normal config defaults.

### 2.3 Profile-path overlay (`tests/profile_path.rs`)

| Case | Expect |
|------|--------|
| Single TOML file via `--profile-path` | New layer name from file stem; built-ins still present |
| Directory tree | Relative paths become layer names (`agents/foo` from `agents/foo.toml`) |

---

## 3. Field tests (the test client)

`isol8-field-test` is a small binary that, for each scenario, builds an **ad-hoc
profile** and an **ad-hoc scratch HOME** under the OS temp dir, runs a probe
command through the real sandbox, and asserts the observed effect. It prints a
human-readable table and exits non-zero if any scenario fails.

### 3.1 Shape of a scenario

```text
scenario     = name + profile (built in-memory) + probe + expected outcome
probe        = a tiny command run inside the sandbox (read a file, write a file,
               print an env var, attempt a network connect)
outcome      = Allowed | Denied | EnvAbsent | EnvPresent  (observed via exit
               code / stdout / created files), compared to expectation
```

The client builds a fresh temp workspace per scenario, so runs are isolated and
leave nothing behind (cleaned on exit; `--keep` to inspect failures).

### 3.2 Baseline scenarios

| # | Profile grant | Probe | Expect |
|---|---------------|-------|--------|
| 1 | (none) | read a file outside any grant | **Denied** |
| 2 | `rw` on workspace | write a file in workspace | **Allowed** |
| 3 | `ro` on a seed dir | write into the seed dir | **Denied** |
| 4 | `ro` on a seed dir | read from the seed dir | **Allowed** |
| 5 | scratch HOME | `$HOME` points at scratch, real home unreadable | **Denied** on real home |
| 6 | env allowlist | a non-allowlisted var (e.g. `SECRET_TOKEN`) | **EnvAbsent** |
| 7 | env allowlist | `PATH` / `HOME` present | **EnvPresent** |
| 8 | (N0, future) | TCP connect to a public host | **Denied** |
| 9 | `rewrite` ensure-arg | run a probe whose injected arg creates a marker file | **Allowed** (file present) |

Scenario 9 builds an ad-hoc layer with a `rewrite` (an external/in-memory profile,
not a built-in), applies it via `profile::apply_rewrite`, and confirms the injected
argument actually reached the executed program under the real sandbox.

Scenarios 1–7 only need the path/env/HOME backend (Phase 1). Network scenarios
are gated behind the net tiers (Phase 3) and skipped with a clear `SKIP` until
then.

### 3.3 Output

```
isol8 field tests — backend: linux/landlock (abi v5)   home: /tmp/isol8-ft-AB12

  PASS  01 deny-read-outside-grant
  PASS  02 rw-workspace-write
  PASS  03 ro-seed-write-denied
  SKIP  08 net-n0-deny           (network tier not implemented)
  ...
  7 passed, 0 failed, 1 skipped
```

Exit code: `0` all passed (skips allowed), `1` any failure. This makes it usable
both interactively and as a CI job.

---

## 4. Cross-platform portability

Field tests must run on **Linux, macOS, WSL2, and Windows** without hard-coded
paths. Two rules:

**(a) All test paths are derived, never literal.** Everything hangs off the OS
temp dir via `std::env::temp_dir()` (honours `TMPDIR` on Unix, `TMP`/`TEMP` on
Windows), with a per-run unique subdir:

```
<temp>/isol8-ft-<rand>/
  home/        # scratch $HOME for the sandboxed probe
  workspace/   # the rw grant target
  seed/        # the ro grant target
  outside/     # control: never granted, must stay inaccessible
```

No test ever references `/home/...`, `/Users/...`, `/etc`, or `C:\...` directly.
A single `fixtures` module resolves these once and hands out `PathBuf`s.

**(b) Platform expectations are declared, not assumed.** A small capability probe
decides, per OS, whether a scenario runs, is expected to enforce, or is skipped:

| Platform | Backend | Field tests |
|----------|---------|-------------|
| Linux (Landlock ≥ ABI 1) | Landlock + namespaces | Run & enforce. |
| Linux (no Landlock) | — | Path scenarios `SKIP` with reason (kernel too old). |
| macOS | Seatbelt (`sandbox-exec`) | Run & enforce. |
| WSL2 | Linux backend (if WSL kernel has Landlock) | Same as Linux; probe decides. |
| Windows | AppContainer (Phase 5) | All `SKIP` until backend exists. |

The probe is the same one `select()` uses in `src/backends/mod.rs`, so field
tests and the real CLI agree on what the current platform can do. A scenario that
*should* enforce but the backend reports unavailable is a **failure**, not a skip
— that catches silent loss of confinement.

### 4.1 Path & separator hygiene

- Build paths with `Path`/`PathBuf` join, never string concatenation with `/`.
- Probe commands are chosen per-OS (e.g. read via a tiny in-process helper rather
  than shelling out to `cat`/`type`) so tests don't depend on platform binaries.
- The scratch HOME env var differs: set `HOME` on Unix, `USERPROFILE` on Windows;
  the fixtures layer abstracts this.

---

## 5. Running

```sh
just test          # unit + integration (all platforms, no privileges)
just field-test    # real-sandbox field tests on this machine
just ci            # fmt-check + clippy -D warnings + build + test (the gate)

# targeted filter / profile coverage:
cargo test profile_filters
cargo test filter::
```

Field tests are intentionally *not* part of `cargo test` by default: they need a
functional backend and the right OS, and are run via their own binary so CI can
schedule them per-platform. CI matrix: unit/integration everywhere; field tests
on Linux and macOS runners.

---

## 6. Conventions

- Every non-trivial logic change ships with a test in the same change (unit for
  logic, a field scenario for an enforcement behaviour).
- A new profile grant type or matcher must add at least one field scenario that
  proves the OS honours it.
- A new filter axis or auto-selection rule must add unit tests in `filter.rs` and
  at least one integration case in `tests/profile_filters.rs` (or extend
  `tests/profile_path.rs` when the behaviour is overlay-specific).
- Tests leave the machine clean: temp dirs removed on exit unless `--keep`.
- Prefer many tiny scenarios over one large one — a failing scenario name should
  point straight at the broken rule.