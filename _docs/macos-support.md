# isol8 — macOS Seatbelt Backend

How the macOS backend works, what it enforces, its requirements, and its limitations.

---

## Overview

On macOS, isol8 enforces isolation via **Apple Seatbelt** (`/usr/bin/sandbox-exec`),
Apple's built-in sandboxing facility, using the **Sandbox Policy Language (SBPL)**.
Every confined process runs under a deny-by-default policy: the generated SBPL begins
with `(deny default)`, and the only paths and operations reachable by the process are
those explicitly granted by the merged profile stack.

This is the **most mature, fully-enforcing backend** in isol8. macOS enforcement has
been validated on real `sandbox-exec` under macOS 12 through macOS 26 (Tahoe).

Key characteristics:

- **Single unprivileged binary.** `sandbox-exec` requires no root, no elevated
  capabilities, and no persistent daemon.
- **Deny-by-default.** Only explicit grants from the merged profile reach the
  confined process; everything else receives `EPERM`.
- **Inline policy.** The rendered SBPL is passed to `sandbox-exec -p` as an inline
  string — no temporary `.sb` file is written to disk.
- **Profile-driven.** All grants, capabilities, and raw SBPL come from TOML profile
  layers; no policy is hard-coded in the backend.

---

## Requirements

| Requirement | Detail |
|-------------|--------|
| macOS 12 or later | `sandbox-exec` ships with the OS; no installation step. |
| `/usr/bin/sandbox-exec` present | Seatbelt is deprecated by Apple but remains present on all current macOS releases. isol8 fails fast with a clear message if it is missing. |
| No root | The binary runs completely unprivileged. |
| No kernel extensions or daemons | `sandbox-exec` is a standard userspace wrapper. |

If `sandbox-exec` is not found at launch, isol8 prints:

```
/usr/bin/sandbox-exec not found. Seatbelt is deprecated but present on
macOS 12+; isol8 requires it for the macOS backend.
```

---

## How the Policy Is Rendered

The merged `Profile` is converted to an SBPL string by `render_policy()` in
`src/backends/macos.rs`. **Order matters** — Seatbelt is last-match-wins, so
rules are emitted in a carefully prescribed sequence.

### Policy shape

```
(version 1)
(deny default)
;; ancestor metadata for path resolution (R2.3)
(allow file-read-metadata (literal "/"))
(allow file-read-metadata (literal "/a"))
…
;; path grants
(allow file-read* (subpath "/a/b"))
(allow file-read* (subpath "/a/b")) (allow file-write* (subpath "/a/b"))   ← rw
(allow file-read-metadata (literal "/exact"))                               ← metadata
;; explicit denies (carve holes; emitted after allows)
(deny file-read* file-write* (subpath "/a/b/.ssh"))
;; macos capabilities
(allow mach-lookup)
…
;; raw SBPL passthrough
(allow mach-lookup (global-name "com.example"))
```

### Section-by-section

**1. Header**

```sbpl
(version 1)
(deny default)
```

Every policy starts with these two lines. `(deny default)` makes the sandbox
deny-by-default before any rule is evaluated.

**2. Ancestor `file-read-metadata` grants (R2.3)**

Path resolution (`stat`, `getcwd`, `open`) stat-walks every ancestor of a granted
path. Without `file-read-metadata` on each ancestor the OS fails the open. For every
path grant (and its symlink-resolved form), isol8 emits one `file-read-metadata`
literal per ancestor directory, deduped across all grants:

```sbpl
(allow file-read-metadata (literal "/"))
(allow file-read-metadata (literal "/home"))
(allow file-read-metadata (literal "/home/user"))
```

Note that `(literal "/")` must always be emitted — every process inherits the root
directory as its cwd from launchd and reads it at startup. Without it, the runtime
aborts with SIGABRT (exit 134) even before the program's first instruction.

**3. Per-grant allows**

For each path grant:

| `access` | Emitted rules |
|----------|---------------|
| `ro` | `(allow file-read* <matcher>)` |
| `rw` | `(allow file-read* <matcher>)` + `(allow file-write* <matcher>)` |
| `metadata` | `(allow file-read-metadata <matcher>)` |
| `none` | Nothing emitted here (deferred to section 4) |

**4. `none` denies — after the allows**

Because Seatbelt is **last-match-wins**, a `none` (explicit deny) grant must be
emitted _after_ the allows that cover the same path, so it can carve a hole out of
a broader grant. For example:

```sbpl
(allow file-read* (subpath "/home/user"))
(allow file-write* (subpath "/home/user"))
;; .ssh carved out:
(deny file-read* file-write* (subpath "/home/user/.ssh"))
```

The deny always names both `file-read*` and `file-write*` explicitly. A bare
`(deny file* …)` does **not** block writes (verified in testing); isol8 never emits
the bare form.

**5. Capabilities**

macOS-only capability allows from `[macos] capabilities`. Each maps to one or more
SBPL operation rules (see the Capabilities section below).

**6. Raw SBPL passthrough**

Any content in `[macos] raw` is appended verbatim as the last section. This is
an escape hatch for Seatbelt operations that isol8's typed model does not cover.

---

## Symlink Dual-Emission

macOS firmlinks and symlinks (`/tmp` → `/private/tmp`, `/var` → `/private/var`,
`/home` → `/System/Volumes/Data/home`) are **not interchangeable** to Seatbelt: a
grant on `/tmp` does not cover accesses via `/private/tmp` and vice versa.

For every path grant, isol8 resolves the authored path to its canonical form using
`std::path::Path::canonicalize` (walking up the tree to handle not-yet-created
subdirectories). If the resolved form differs from the authored form, **both** are
emitted in the same rule:

```sbpl
(allow file-read* (subpath "/tmp") (subpath "/private/tmp"))
```

This handles the common case where tools write to `/tmp` but the OS internally
accesses `/private/tmp` (e.g. files created under `/var/folders`).

---

## Match Kinds

Seatbelt supports four native path matcher types. isol8's `match` field maps to them
directly:

| `match` value | SBPL matcher | Behavior |
|---------------|-------------|----------|
| `subpath` (default) | `(subpath "…")` | The named directory and everything recursively beneath it. |
| `literal` | `(literal "…")` | Exactly this one path node. |
| `prefix` | `(regex "^<escaped>")` | Anchored regex approximation of a string prefix. |
| `regex` | `(regex "…")` | Verbatim SBPL regex pattern. |

`prefix` has no native Seatbelt matcher; isol8 approximates it by emitting an
anchored regex with all regex metacharacters in the path escaped. This is documented
as approximate in `_docs/profile-model.md §5`.

All four match kinds apply to symlink dual-emission: both the authored path and the
resolved path are matched using the same `match` kind.

---

## Capabilities

The `[macos]` table in a profile layer can list typed capabilities and raw SBPL. The
`capabilities` array accepts these values:

| Capability | SBPL rule emitted | Notes |
|------------|------------------|-------|
| `mach-lookup` | `(allow mach-lookup)` | Mach service name lookups via the bootstrap server. |
| `mach-register` | `(allow mach-register)` | Register a Mach service name. |
| `iokit-open` | `(allow iokit-open)` | Open IOKit user-client connections. |
| `sysctl-read` | `(allow sysctl-read)` | Read sysctl values. |
| `process-exec` | `(allow process-exec*)` | Execute other processes. |
| `process-fork` | `(allow process-fork)` | Fork child processes. |
| `process-info` | `(allow process-info*)` | Query process info (e.g. `sysctl proc_info`). |
| `signal` | `(allow signal)` | Send signals to other processes. |
| `pseudo-tty` | `(allow pseudo-tty)` | Allocate a pseudo-terminal device. |
| `user-preference-read` | `(allow user-preference-read)` | Read `CFPreferences` / `NSUserDefaults`. |
| `user-preference-write` | `(allow user-preference-write)` | Write `CFPreferences` / `NSUserDefaults`. |
| `ipc-posix-shm` | `(allow ipc-posix-shm*)` | POSIX shared-memory operations. |
| `sysv-sem` | `(allow ipc-sysv-sem)` | System V semaphore operations. |
| `pasteboard` | `(allow mach-lookup (global-name "com.apple.pboard.service"))` | Clipboard access. See note below. |

**Pasteboard note.** `pasteboard` is not a Seatbelt operation class. Clipboard access
is mediated by a Mach service lookup to `com.apple.pboard.service`, so that is what
isol8 emits. A bare `(allow pasteboard)` does not compile under real `sandbox-exec`.

**Raw SBPL.** For Seatbelt operations beyond the typed list, set `[macos] raw` to
verbatim SBPL. Content is appended after the generated rules:

```toml
[macos]
capabilities = ["mach-lookup"]
raw = """
(allow mach-lookup (global-name "com.apple.CoreServices.coreservicesd"))
"""
```

---

## Minimal Allow-Set

A trivial command (e.g. `/bin/sh -c 'echo hi'`) requires at least the following
rules beyond `(deny default)` to start without aborting:

```sbpl
(version 1)
(deny default)
(allow process-exec*)
(allow process-fork)
(allow file-read* (subpath "/usr/lib") (subpath "/System") (subpath "/bin") (literal "/"))
```

Key items:
- `(literal "/")` — mandatory; every process reads the root directory at startup.
- `/System` — must be the whole subtree; it contains the dyld shared cache under
  `/System/Volumes/Preboot/Cryptexes`.
- `/usr/lib` — dylibs.
- `/bin`, `/usr/bin` — the binary itself and any interpreter.

The built-in `base` and `macos/system-runtime` layers embed these grants; normal runs
do not need to specify them manually.

---

## HOME (R4) and Environment (R3)

### HOME resolution (R4)

The effective `$HOME` is resolved **before** any path grant computation. Precedence
(highest first):

1. `--home <PATH>` CLI flag
2. `home_replace` in a profile layer
3. The real home directory (default — HOME is not replaced unless explicitly configured)

The `#HOME` token in profile path grants expands to the **real** home directory even
when a replacement is active, allowing profiles to reference the real home without
hardcoding it.

When a scratch home is used with `home_replace.seed = true`, isol8 copies a minimal
set of files from the real home into the scratch home on first creation. `--no-seed`
suppresses this.

### Environment sanitization (R3)

The confined process receives a minimal sanitized environment. isol8 starts from a
fixed allowlist and applies overrides in order:

1. **Minimal allowlist:** `HOME`, `PATH`, `SHELL`, `TMPDIR`, `USER`, `LOGNAME`, `PWD`.
   Secrets and arbitrary host variables do not pass through.
2. **`HOME`** is set to the effective home (resolved in step 1 above).
3. **Profile `[env]` defaults** — env defaults from merged layers.
4. **`--env-pass <NAME>`** — pass a named host variable through.
5. **`--set-env <K=V>`** — set a variable explicitly (highest precedence); cannot
   override the injected `ISOL8_SANDBOXED` marker.

---

## Exit Codes and Diagnostics

`sandbox-exec` overloads several exit codes for its own use. isol8 interprets them
and surfaces a clear error in each case:

| Exit code | Meaning | isol8 message |
|-----------|---------|---------------|
| 64 | Usage error (bad `sandbox-exec` invocation) | "sandbox-exec reported a usage error (exit 64). Check that the confined command and arguments are valid." |
| 65 | Policy compile error (SBPL rejected by the kernel) | "sandbox-exec rejected the generated Seatbelt policy (exit 65). This is a policy-compile error, not the command failing." Includes the generated policy text for inspection. |
| 71 | Command not executable inside sandbox | "could not run \"cmd\": the command is missing or not executable inside the sandbox." |
| 134 | SIGABRT — usually a launch abort | "the confined command aborted (exit 134 / SIGABRT). This usually means isol8 is running inside another sandbox that forbids nesting, or the policy denies read access to '/'." |
| anything else | The confined program's own exit code | Passed through as-is. |

For exit 65 (policy compile error), the generated SBPL is printed in the error
message. Re-run with `--show-policies` to inspect the effective policy independently.

For exit 134 (SIGABRT launch abort), the most common cause is a missing path grant
that the runtime needs before the first user instruction — use `isol8 @diag` (below)
to locate it automatically.

---

## `isol8 @diag` — Launch-Abort Diagnoser

A SIGABRT launch abort produces no output and no diagnostic message. `@diag` finds
the missing grant automatically through **dichotomic minimization (delta-debug)**:

**Algorithm:**

1. Render the command's real effective policy (the one causing the abort).
2. Confirm the command launches when read access to every top-level directory on `/`
   is added. If it still aborts, the issue is likely a capability or network
   requirement rather than a path — `@diag` exits with a clear message in that case.
3. Dichotomic minimization: repeatedly split the candidate grant set in half and
   re-run the command under each trial policy until only the grants whose absence
   causes the abort remain. Each trial uses a 10-second timeout; a command still
   running after 10 seconds is counted as "launched" (so use a fast-exiting probe
   like `--version`).

**Example session:**

```sh
isol8 @diag node --version
# == isol8 @diag: node --version ==
#
# 'node --version' is aborted at launch by the current sandbox policy. Searching for the missing grant…
#
# Found it in 5 trials. 'node --version' launches once the sandbox grants read access to:
#
#   /
#
# or add to a profile layer:
#   { path = "/", access = "ro", match = "literal" }
```

`@diag` is macOS-only (it drives `sandbox-exec`). On other platforms it exits with
an error.

**Important:** `(literal "/")` is recommended over `--add-dirs-ro /` for root-only
grants. The `--add-dirs-*` flags emit `(subpath "/")`, which would grant the entire
filesystem; the profile `match = "literal"` form is the safe way to allow only the
root directory node.

---

## Usage Examples

### Run a command with project write access

```sh
isol8 --add-dirs-rw "$PWD" -- make build
```

### Inspect the effective Seatbelt policy before running

```sh
isol8 --show-policies echo hi
isol8 --show-policies --profile agents/claude-code claude --version
```

`--dry-run` is an alias for `--show-policies`.

### Confine an AI agent with auto-selected layers

With `auto_profiles = true` in config (the `@init` default):

```sh
isol8 --show-profiles claude --version    # preview which layers apply
isol8 --add-dirs-rw "$PWD" claude         # run confined
```

### Diagnose a launch abort

```sh
isol8 @diag node --version
# Reports the missing path grant and how to add it.
```

### Use the Rust embedding API

```rust
let exit: i32 = isol8::Sandbox::new()
    .profile("base")
    .grant_rw("/my/project")
    .run(["make", "build"])?;
```

See `_docs/instructions.md` for the full `Sandbox` builder API and all CLI flags.

---

## Known Limitations

**Developer tool paths.** Tools like `git`, `cargo`, and Xcode toolchain binaries
need paths beyond `macos/system-runtime` — developer tool shims, command-line tool
paths under `/Library/Developer`, keychain access, and so on. Use
`--profile toolchains/rust` or `--profile integrations/git`, or add the missing
paths with `--add-dirs-ro`. The `@diag` tool is useful for finding exactly which
paths are needed.

**Nested sandboxing is unsupported.** Seatbelt cannot be nested: if isol8 itself is
already running inside a Seatbelt sandbox (e.g. another isol8 invocation or an app
with a sandbox profile), the inner `sandbox-exec` will abort with SIGABRT (exit 134).
The error message reports this. The `Error::NestedSandbox` variant is available in
the library API.

**Seatbelt is deprecated by Apple.** `sandbox-exec` and the SBPL language are
officially deprecated. Apple has not removed them through macOS 26, and no replacement
public API exists for unprivileged per-process sandboxing. This is a long-term risk;
if Apple removes the tool in a future macOS release, the macOS backend will need to be
rewritten using a different mechanism (likely Endpoint Security or App Sandbox, both of
which have significant privilege or distribution requirements).

**No network enforcement.** Network isolation tiers N2 and N3 (rootless pasta and
netns/nftables) are not yet implemented. The current macOS backend enforces only
filesystem access and the listed capability operations; network calls are unrestricted.

**`match = prefix` is approximate.** Seatbelt has no native prefix matcher. isol8
approximates it with an anchored regex (`^<escaped-path>`), which covers the common
cases but is not guaranteed to be byte-identical to a true prefix match in all edge
cases (see `_docs/profile-model.md §5`).

**Path escaping edge cases.** SBPL strings use backslash escaping. Paths containing
literal backslashes or double quotes are escaped correctly, but such paths are rare on
macOS and have limited test coverage.

---

## Related Documents

| Document | Contents |
|----------|----------|
| [`_docs/instructions.md`](./instructions.md) | Full CLI reference, flags, configuration, examples |
| [`_docs/profile-model.md`](./profile-model.md) | Profile TOML format, `match` kinds, merge rules, capabilities |
| [`_docs/linux-support.md`](./linux-support.md) | Linux Landlock backend (the macOS counterpart) |
| [`_docs/testing-strategies.md`](./testing-strategies.md) | Unit and field test coverage |
| [`AGENTS.md`](../AGENTS.md) | Project goals (R1–R6), architecture, current status |
