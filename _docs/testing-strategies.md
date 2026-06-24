# isol8 — Testing Strategies

How `isol8` is verified. Two layers: **unit/integration tests** (fast, in-process,
`cargo test`) and **field tests** (a standalone client that spawns the *real*
sandbox against an ad-hoc home + profile and reports what was actually allowed or
denied). Field tests are the ground truth — a profile is only correct if the OS
enforces it.

> Status: implemented. Unit + integration tests (`cargo test`) and the
> field-test binary `src/bin/isol8-field-test.rs` (`just field-test`) are in place
> and green on macOS, Linux (WSL2), and **Windows (GNU toolchain + MinGW-w64)**.
> On macOS/Linux scenarios 1–9 enforce path + env. On Windows, scenarios **01–07**
> enforce path grants when `isol8-winhook.dll` is deployed (hook mode); scenario
> **08** skips until net tiers land; scenario **09** verifies AppContainer spawn;
> scenario **10** verifies hook policy propagates to **grandchild** processes
> spawned by the confined child.
> Linux-specific scenarios **10–16** compile only on Linux.
> Scenario **17** (Linux only) is a regression test for finding C1: a zero-explicit-grant
> profile must still enforce deny-all via Landlock (the former early-return bug would
> silently skip `restrict_self()` and leave the process unconfined).
> See [`AGENTS.md`](../AGENTS.md).

---

## 1. Layers at a glance

| Layer | Where | What it proves | Runs on |
|-------|-------|----------------|---------|
| Unit | `src/**` `#[cfg(test)]` | Pure logic: profile merge, `requires` resolution, env allowlist, HOME-first resolution (default real home; replacement opt-in), executable resolution + clean "command not found", filter matching, `--show-policies` rendering. | All platforms, no privileges. |
| Integration | `tests/*.rs` | Crate wired end-to-end *without* exec: load profiles → select layers → filter → resolve → merge → render. `tests/windows_spawn.rs` spawns real hook-mode children on Windows. | All platforms (Windows spawn tests require hook DLL). |
| Field | `src/bin/isol8-field-test.rs` | The OS actually enforces the policy: denied paths fail, granted paths work, env is sanitized, AppContainer spawn succeeds (Windows). | Per-OS, best-effort, prints a report. |

Unit and integration tests never touch the real filesystem outside a temp dir and
never require the backend to be functional. Field tests require a working backend
(Landlock on Linux, Seatbelt on macOS, hook DLL or AppContainer on Windows) and
degrade gracefully where enforcement is unavailable.

---

## 2. Unit & integration tests

Standard `cargo test`. Keep them deterministic and platform-independent:

- **Profile merge** — deny-first union, highest-layer-explicit-grant-wins, env
  defaults, network domain union. (`tests/profile_merge.rs`, `src/profile.rs`)
- **Inheritance** — `requires`/`extends` DFS: deps-first topo order, cycle
  detection, dedup, selection-order tiebreak. (`tests/profile_merge.rs`,
  `src/profile.rs`)
- **Env construction** — only the allowlist survives; HOME override applied first;
  `--env-pass NAME` pulls a host var through and `--set-env K=V` overrides profile
  defaults, neither able to clear the `ISOL8_SANDBOXED` guard; malformed `--set-env`
  errors instead of being silently dropped. On Windows, allowlist matching is
  case-insensitive (`Path` → `PATH`).
  (`src/env.rs::cli_env_pass_and_set_override_profile`,
  `src/resolve.rs::parse_set_env_pairs_and_errors`,
  `src/env.rs::windows_home_vars_follow_effective_home`)
- **HOME resolution** — replacement is **opt-in**: no `--home`/`home_replace` → the
  real home; a layer's `home_replace` (path or `auto_scratch`) or `--home` overrides
  it, with `~` expanded against the real home. (`src/home.rs`,
  `tests/profile_filters.rs::default_run_keeps_real_home`,
  `profile_home_replace_overrides_home`)
- **Seeding & `--no-seed`** — seeding is **first-creation-only**: re-seeding over an
  existing read-only copy doesn't error and keeps the first snapshot; `--no-seed`
  clears every layer's seed list for the run. (`src/home.rs::seed_is_first_creation_only`,
  `no_seed_clears_seed_list`)
- **Seed security (C3)** — seed entries with `..` path components or absolute paths are
  rejected (`src/home.rs::seed_rejects_dotdot_traversal`); symlinks at any depth of
  recursion are silently skipped, preventing follow-through to targets outside `$HOME`
  (`src/home.rs::seed_skips_symlinks`).
- **`#HOME` token** — expands to the **real** home before `~` expansion, so a grant
  survives an active `--home`/`home_replace`; with no replacement it coincides with
  `~`. (`src/home.rs::expand_grant_real_home_token`)
- **Layer-stack provenance** — the resolved (deps-first) stack tags each layer
  `explicit` / `auto` / `required`, matching what actually contributes grants.
  (`tests/profile_filters.rs::layer_stack_tags_provenance_explicit_auto_required`)
- **Executable resolution** — `cmd[0]` resolved execvp-style against the host `PATH`
  to an absolute path; missing → clean `command "x" not found`; backslash paths on
  Windows; the resolved binary is auto-granted `ro`. Applied on the run/`@diag` exec
  paths only, so introspection (`--show-policies`) stays pure for not-yet-installed
  commands. (`src/resolve.rs`,
  `tests/profile_filters.rs::confine_executable_absolutizes_and_grants_binary`)
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
involved for most tests, so they are the portable backbone of CI.

### 2.1 Filter unit tests (`src/filter.rs`)

Filter logic is pure and tested in-process:

| Case | Expect |
|------|--------|
| `executable_basename` | `/usr/bin/claude` → `claude` (path and `.exe` stripped) |
| `filter_matches` AND semantics | All non-empty axes (`os`, `arch`, `executables`) must match |
| Executable match | Basename **or** full argv[0] literal (e.g. `/opt/bin/claude`) |
| `is_auto_selectable` | Only layers with `filter.executables` are auto-candidates |
| `apply_layer_filter` | OS/arch mismatch → empty paths/env/macos/windows; `requires` kept |
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
| Default HOME | `effective_policy` for default stack | `home.path` is the real home (no replacement) |
| Profile HOME change | overlay layer with `home_replace` | `home.path` follows the profile, not the real home |
| Layer-stack provenance | name the OS alias + `auto_profiles` + `claude` cmd | stack tags `base` `required`, alias `explicit`, `agents/claude-code` `auto`; deps-first order |
| Executable confinement | `confine_executable` on `/bin/sh` or `%SYSTEMROOT%\System32\cmd.exe` | `cmd[0]` absolutized; resolved binary auto-granted `ro` |

Default profile stacks in these tests use `base` plus the OS-appropriate
`macos/system-runtime`, `linux/system-runtime`, or `windows/system-runtime` layer
so behaviour matches normal config defaults.

### 2.3 Profile-path overlay (`tests/profile_path.rs`)

| Case | Expect |
|------|--------|
| Single TOML file via `--profile-path` | New layer name from file stem; built-ins still present |
| Directory tree | Relative paths become layer names (`agents/foo` from `agents/foo.toml`) |

### 2.4 Windows-only unit tests

| Module | Test | Expect |
|--------|------|--------|
| `backends::windows` | `empty_env_block_is_double_null` | Empty env → `[0, 0]` UTF-16 block |
| `backends::windows` | `env_block_encodes_sorted_entries` | Sorted `KEY=VAL\0` pairs |
| `backends::windows` | `quote_arg_spaces_and_quotes` | MSDN `CommandLineToArgvW` quoting |
| `backends::windows` | `quoted_command_line_joins_args` | Space-containing paths quoted |
| `backends::windows_policy` | `ro_seed_overrides_parent_rw`, `seed_data_txt_readable_with_ro_grant` | JSON path policy deny-first merge |
| `backends::windows_hook` | `hook_dll_name_is_stable` | DLL discovery name is `isol8-winhook.dll` |
| `backends::windows_hook` | `hook_dll_search_paths_are_all_absolute` | **C2 regression**: search paths never include a bare filename (no CWD search) |
| `isol8-path-policy` | `dotdot_traversal_is_denied` | `..` in path → deny (fail-closed) |
| `isol8-path-policy` | `unc_grant_is_case_insensitive` | UNC grants match case-insensitively |
| `isol8-path-policy` | `prefix_rank_uses_normalized_grant_length` | Longest-prefix uses normalized grant length |
| `isol8-winhook` | `generic_read_with_synchronize_is_not_write` | Read opens not classified as write |
| `isol8-winhook` | `delete_access_is_write`, `read_ea_is_not_write` | `WRITE_MASK` includes `DELETE`, excludes `FILE_READ_EA` |
| `home` | `expand_windows_vars_substitutes_systemroot` | `%SYSTEMROOT%` → absolute path |
| `env` | `windows_home_vars_follow_effective_home` | `USERPROFILE`/`APPDATA`/… follow `HOME` |
| `resolve` | `windows_absolute_path_with_backslashes` | `C:\...\cmd.exe` resolves without PATH search |
| `filter` | `apply_layer_filter_clears_windows_on_os_mismatch` | Wrong OS → `windows` block cleared |
| `filter` | `apply_policies_folds_windows_caps` | Conditional `[[policies]].windows` folded |

`tests/windows_spawn.rs` (integration, Windows only): spawns confined `isol8-probe`
children through `Backend::spawn` and asserts ro-seed read succeeds when the hook
DLL is present; `grandchild_inherits_hook_policy` verifies scenario 10 behaviour.

---

## 3. Field tests (the test client)

`isol8-field-test` is a small binary that, for each scenario, builds an **ad-hoc
profile** and an **ad-hoc scratch HOME** under the OS temp dir, runs a probe
command through the real sandbox, and asserts the observed effect. It prints a
human-readable table and exits non-zero if any scenario fails.

Each run calls `confine_executable` before spawn (matching the real CLI pipeline).

On Windows, path probes use `isol8-probe` (not `cmd.exe` quoting) and require
`isol8-winhook.dll` beside the `isol8-field-test` / `isol8` binary. Run
`just build-windows-test-deps` or `just field-test-windows` to build and deploy
the DLL automatically.

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

| # | Profile grant | Probe | Expect (macOS/Linux) | Expect (Windows, hook DLL present) |
|---|---------------|-------|----------------------|-------------------------------------|
| 1 | (none) | read a file outside any grant | **Denied** | **Denied** |
| 2 | `rw` on workspace | write a file in workspace | **Allowed** | **Allowed** |
| 3 | `ro` on a seed dir | write into the seed dir | **Denied** | **Denied** |
| 4 | `ro` on a seed dir | read from the seed dir | **Allowed** | **Allowed** |
| 5 | profile-requested scratch HOME | real home unreadable | **Denied** on real home | **Denied** |
| 6 | env allowlist | non-allowlisted `SECRET_TOKEN` | **EnvAbsent** | **EnvAbsent** (`cmd.exe /c if defined …`) |
| 7 | env allowlist | `PATH` / `HOME` present | **EnvPresent** | **EnvPresent** |
| 8 | (N0, future) | TCP connect to a public host | **SKIP** | **SKIP** |
| 9 | `rewrite` ensure-arg (Unix) / AppContainer spawn (Windows) | injected arg creates file / `cmd.exe /c exit 0` | **Allowed** (rewrite) | **Allowed** (spawn smoke test) |
| 10 | (none) | grandchild read outside grant (`isol8-probe spawn read <outside>/secret.txt`) | — | **Denied** (hook reinject on `CreateProcess*`) |
| 17 *(Linux only)* | zero explicit grants | read a file in `outside/` | **Denied** (Landlock deny-all, or library load failure) | — |

On Unix, scenario 9 builds an ad-hoc layer with a `rewrite`, applies it via
`profile::apply_rewrite`, and confirms the injected argument reached the executed
program under the real sandbox.

On Windows, scenario **09 appcontainer-spawn** verifies `CreateAppContainerProfile`
+ `SECURITY_CAPABILITIES` + `CreateProcessW` can launch `cmd.exe` and return exit
code 0 when the hook DLL is absent (AppContainer-only path). Path scenarios 01–07
use **hook mode** when `isol8-winhook.dll` is found.

Scenario **10 grandchild-deny-outside-grant** (Windows only) exercises subprocess
escape (review finding H2). The confined parent runs `isol8-probe spawn read
<outside>/secret.txt`, which calls `CreateProcessW` to launch a **grandchild**
that attempts the same denied read. The hook DLL detours
   `kernelbase!CreateProcessInternalW` / `CreateProcessA`, injects itself into every
   descendant, and inherits
`ISOL8_PATH_POLICY` via the environment — so the grandchild must fail the read
even though only the direct child was launched by isol8. Without the hook DLL,
scenario 10 is **SKIP**.

Without the hook DLL, Windows path scenarios **01–05** and **10** are **SKIP**
(AppContainer path grants are documentary only; no hook reinject).

Scenarios 1–7 only need the path/env/HOME backend (Phase 1). Network scenarios
are gated behind the net tiers (Phase 3) and skipped with a clear `SKIP` until
then.

**Why some features add no new scenario.** `--env-pass` / `--set-env`, the `#HOME`
token, `--no-seed`, and layer-stack provenance are all *resolution-time* logic: they
shape the env map or the absolute path grants that scenarios 2/4 (absolute-path grant
enforcement) and 6/7 (env actually delivered to the child) already prove the OS
honours. The new logic is therefore covered by unit/integration tests (§2), and a
fresh field scenario would only re-exercise the already-proven substrate. A field
scenario is still mandatory for any **new grant type or matcher** (§6).

### 3.3 Output

```
isol8 field tests — platform: windows   home: C:\Users\...\Temp\isol8-ft-12345\home

  PASS  01 deny-read-outside-grant
  PASS  02 rw-workspace-write
  PASS  03 ro-seed-write-denied
  PASS  04 ro-seed-read
  PASS  05 real-home-denied
  PASS  06 env-secret-absent
  PASS  07 env-path-home-present
  SKIP  08 net-n0-deny           (network tier not implemented)
  PASS  09 appcontainer-spawn   (AppContainer CreateProcessW smoke test)
  PASS  10 grandchild-deny-outside-grant   (grandchild read outside grant must fail)

  9 passed, 0 failed, 1 skipped
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
<temp>/isol8-ft-<pid>/
  home/        # scratch $HOME for the sandboxed probe
  workspace/   # the rw grant target
  seed/        # the ro grant target
  outside-<id>/  # control: never granted (sibling of root, not under it)
```

No test ever references `/home/...`, `/Users/...`, `/etc`, or `C:\...` directly
except via `%SYSTEMROOT%` expansion in profiles or resolving the host `cmd.exe`.

**(b) Platform expectations are declared, not assumed.** Field tests set
`path_enforced` from the backend: macOS/Linux always; Windows when
`backends::path_enforcement_available()` (hook DLL found).

| Platform | Backend | Field tests |
|----------|---------|-------------|
| Linux (Landlock ≥ ABI 1) | Landlock + namespaces | Run & enforce paths + env. |
| Linux (no Landlock) | — | Path scenarios `SKIP` with reason (kernel too old). |
| macOS | Seatbelt (`sandbox-exec`) | Run & enforce paths + env. |
| WSL2 | Linux backend (if WSL kernel has Landlock) | Same as Linux; probe decides. |
| Windows (hook DLL present) | Hook mode (`isol8-winhook.dll`) | **01–07, 10** enforce path + env; **09** AppContainer spawn smoke test. Requires MinGW-w64 `gcc` on PATH for `x86_64-pc-windows-gnu` (see §5.1). |
| Windows (no hook DLL) | AppContainer only | **06, 07, 09** enforce; **01–05, 10 skip** (R2 documentary; no subprocess hook). |

The probe is the same one `select()` uses in `src/backends/mod.rs`, so field
tests and the real CLI agree on what the current platform can do. A scenario that
*should* enforce but the backend reports unavailable is a **failure**, not a skip
— that catches silent loss of confinement.

### 4.1 Path & separator hygiene

- Build paths with `Path`/`PathBuf` join, never string concatenation with `/`.
- Probe commands on Windows use `isol8-probe read|write <path>` for path scenarios;
  `cmd.exe /c` for env and AppContainer spawn checks.
- `build_minimal` sets authoritative `HOME`; on Windows `apply_windows_home_vars`
  also sets `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `HOMEDRIVE`, `HOMEPATH`.

---

## 5. Running

```sh
just test                    # unit + integration (all platforms, no privileges)
just field-test              # real-sandbox field tests on this machine
just field-test-windows      # Windows: build hook DLL + probe, then field-test
just build-winhook           # build isol8-winhook.dll beside isol8.exe
just ci                      # fmt-check + clippy -D warnings + build + test (the gate)

# targeted filter / profile coverage:
cargo test profile_filters
cargo test filter::
cargo test backends::windows   # Windows-only (cfg-gated)
cargo test --test windows_spawn  # Windows hook spawn integration
cargo test -p isol8-winhook    # hook access-mask unit tests
```

### 5.1 Windows build prerequisites

The default Rust toolchain on Windows is often `x86_64-pc-windows-gnu`. You need
**MinGW-w64** (`gcc`) on `PATH` to link the main binary and the hook DLL:

```powershell
# Example: MSYS2 UCRT64
# Add C:\msys64\ucrt64\bin to PATH, then:
cargo build -p isol8-winhook
copy target\debug\isol8_winhook.dll target\debug\isol8-winhook.dll
cargo test
cargo run --bin isol8-field-test
# or:
just field-test-windows
```

Alternative: `x86_64-pc-windows-msvc` with **Visual Studio Build Tools** + Windows
SDK (`link.exe` + `kernel32.lib`). The repo ships `.cargo/config.toml` pointing
the GNU linker at `gcc` when present.

### 5.2 Release artifacts

GitHub Releases (`.github/workflows/release.yml`) ship platform zips built by
`_devops/scripts/package-release.sh`:

| Zip | Contents |
|-----|----------|
| `linux-x64.zip` | `isol8` |
| `macos-arm64.zip` | `isol8` |
| `windows-x64.zip` | `isol8.exe` + `isol8-winhook.dll` |

The Windows release job installs MinGW via `msys2/setup-msys2`, builds `isol8` and
`isol8-winhook`, and renames the DLL to `isol8-winhook.dll` (the runtime search
name). Local release: `just release-windows`.

Field tests are intentionally *not* part of `cargo test` by default: they need a
functional backend and the right OS, and are run via their own binary so CI can
schedule them per-platform. CI matrix: unit/integration everywhere; field tests
on Linux, macOS, and Windows runners.

---

## 6. Conventions

- Every non-trivial logic change ships with a test in the same change (unit for
  logic, a field scenario for an enforcement behaviour).
- A new profile grant type or matcher must add at least one field scenario that
  proves the OS honours it (or an explicit `SKIP` with documented reason on
  platforms where enforcement is deferred).
- A new filter axis or auto-selection rule must add unit tests in `filter.rs` and
  at least one integration case in `tests/profile_filters.rs` (or extend
  `tests/profile_path.rs` when the behaviour is overlay-specific).
- Tests leave the machine clean: temp dirs removed on exit unless `--keep`.
- Prefer many tiny scenarios over one large one — a failing scenario name should
  point straight at the broken rule.