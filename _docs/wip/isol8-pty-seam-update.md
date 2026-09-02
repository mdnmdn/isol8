# isol8 — a pseudo-terminal seam for hosts

> **Status:** requirement + proposed design, for isol8 v0.3.x. Written from `refs/isol8/`
> (v0.3.0, `dc280e71c26ee64cd9b6c84f42a3dacd5a1312ba`) while wiring `agent-manager` and Ubiq onto
> the embedding API.
>
> **Audience:** whoever implements this in isol8. Everything below is stated against real symbols in
> that tree; the last section lists every file to open.

## 1. The requirement

Two hosts need to run a **confined process inside a pseudo-terminal they own**:

| Host | Where | What it owns |
|---|---|---|
| `agent-manager` (`am`) | `crates/agent-manager/src/run.rs` | a `portable-pty` pair, the tty pump, `SIGWINCH`, the exit code |
| Ubiq | `crates/ubiq-host/src/pty/mod.rs` | one pty per pane, a reader thread, resize, kill, reap |

An interactive agent harness is not a filter over stdout. It drives a full screen: alternate screen,
absolute cursor addressing, raw keystrokes, resize redraw. It therefore needs a **controlling
terminal**, and its geometry has to reach the kernel so the process is signalled — a pane that
resizes while its harness believes the old size is the classic corruption bug.

**v0.3.0 cannot do this.** `Backend::spawn` sets no stdio, so the child inherits isol8's own; every
`SandboxChild` constructor (`process`, `forked`, `exited`, the Windows one) is `pub(crate)`, so a
host can neither hand isol8 a pty slave nor wrap a child it spawned itself. The escape hatches are
asymmetric and only one of them exists:

- **macOS:** a host *can* work around it — `resolve::effective_policy_in` plus
  `backends::select().render_policy(&profile)` yields the real SBPL, and the host spawns
  `/usr/bin/sandbox-exec -p <policy> -- <cmd>` under its own pty. This works, and is what
  `agent-manager` does as a temporary measure until this seam lands.
- **Linux:** no workaround. Landlock must be applied *inside* the target process between `fork` and
  `exec`; `backends::linux::render_policy` is `pub(crate)` and emits a human-readable comment dump
  (`;; RO <path> Subpath`), not a policy artifact. A host cannot obtain "the rules" or "an argv".

### 1.1 Why not a supervisor shim

The obvious workaround is for the host to make *its own binary* the pty child, and have that child
call `Sandbox::from_spec(spec).spawn()` — the confined grandchild then inherits the shim's stdio,
which is the pty. It works on both platforms and needs nothing from isol8. It is still the wrong
answer:

- **macOS `sandbox-exec` `execve`s in place** (it calls `sandbox_init`, then execs the command), and
  **Linux forks exactly once**. So without a shim there is **one process per pane** — a pid the host
  can `kill` and `wait` on directly, exactly like an unconfined pane.
- A shim inserts a second process permanently. Closing a tab kills the shim and **orphans the
  confined grandchild** unless the shim forwards every signal; the harness's exit code has to be
  relayed rather than read; and `PaneExited` becomes a claim about the supervisor rather than about
  the agent.

A one-time change in isol8 removes an ongoing correctness burden from every host. That is the trade
this document proposes.

## 2. The design — a primitive plus a convenience

Two entry points: one that takes descriptors (for a host that already has a pty), one that makes the
pty itself (for a host that just wants a terminal). The second is a thin wrapper over the first.

```rust
// isol8-core::sandbox

/// Where a confined child's standard streams come from.
#[non_exhaustive]
pub struct SandboxStdio {
    pub stdin: OwnedFd,
    pub stdout: OwnedFd,
    pub stderr: OwnedFd,
    /// `setsid()` + `ioctl(TIOCSCTTY)` on `stdin` in the child, before exec.
    pub controlling_terminal: bool,
}

impl SandboxStdio {
    /// The three streams from one tty slave (dup'd), `controlling_terminal = true`.
    pub fn from_tty(slave: OwnedFd) -> Result<Self>;
    /// Three explicit descriptors, no controlling terminal.
    pub fn from_fds(stdin: OwnedFd, stdout: OwnedFd, stderr: OwnedFd) -> Self;
}

#[derive(Clone, Copy, Debug)]
pub struct PtySize { pub cols: u16, pub rows: u16 }

/// A confined child plus the master side of the terminal it runs on.
pub struct PtyChild { /* SandboxChild + master OwnedFd */ }

impl PtyChild {
    pub fn child(&mut self) -> &mut SandboxChild;
    pub fn master(&self) -> BorrowedFd<'_>;
    pub fn into_parts(self) -> (SandboxChild, OwnedFd);

    pub fn resize(&self, size: PtySize) -> Result<()>;   // TIOCSWINSZ on the master
    pub fn get_size(&self) -> Result<PtySize>;           // TIOCGWINSZ
    pub fn try_clone_reader(&self) -> Result<File>;      // dup of the master
    pub fn take_writer(&mut self) -> Result<File>;       // dup of the master
}

impl Sandbox {
    pub fn spawn_with_stdio(self, cmd: I, stdio: SandboxStdio) -> Result<SandboxChild>;
    pub fn spawn_pty(self, cmd: I, size: PtySize) -> Result<PtyChild>;
}

// Hermetic, Context-explicit variants, matching resolve::effective_policy_in / sandbox::dry_run_in:
pub fn spawn_with_stdio_in(spec: &Spec, ctx: &Context, stdio: SandboxStdio) -> Result<SandboxChild>;
pub fn spawn_pty_in(spec: &Spec, ctx: &Context, size: PtySize) -> Result<PtyChild>;
```

`spawn_pty` is: `openpty(size)` → `SandboxStdio::from_tty(slave)` → `spawn_with_stdio` → **drop the
slave**, so the master sees EOF when the harness exits. Both entry points keep the existing pipeline
verbatim — `ensure_not_nested`, `effective_policy(_in)`, `home::materialize`, `confine_executable` —
and differ only in how the backend is asked to spawn.

### 2.1 Why `PtyChild` carries a reader, a writer and a resize

Handing back a bare fd is not enough, because **`portable-pty` 0.9 cannot adopt a foreign master.**
Verified against `portable-pty-0.9.0`: the unix `openpty` free function is private, `UnixMasterPty`
and `UnixSlavePty` are not `pub`, their fields are private, `PtySystem::openpty` always mints its own
pair, and `SlavePty` exposes exactly one method (`spawn_command`) — so neither side's descriptor can
be borrowed or injected. `MasterPty::as_raw_fd` only *reads* a fd it already owns.

So a host given only an `OwnedFd` must write its own `Read`/`Write` wrapper and its own
`ioctl(TIOCSWINSZ)`, pulling `libc` and `unsafe` into a crate that has neither — once per host, four
times across the two consumers. `try_clone_reader`, `take_writer` and `resize` are what make the
seam usable by a host that only wants bytes, and they are the same three calls `MasterPty` already
offers, so a host can keep one internal abstraction over both paths.

## 3. Backend work

`Backend` gains one method. Keep `spawn` as it is (it becomes `spawn_with_stdio` with inherited
stdio, or stays a separate path — implementer's choice):

```rust
fn spawn_with_stdio(
    &self,
    profile: &Profile,
    env: &HashMap<String, String>,
    cmd: &[String],
    stdio: SandboxStdio,
) -> Result<SandboxChild>;
```

`Backend` is closed to external implementation (`SandboxChild`'s constructors are `pub(crate)`), so
adding a method is not a breaking change for embedders.

**macOS** — `crates/isol8-core/src/backends/macos.rs`, `MacosBackend::spawn`. The command is already
`Command::new("/usr/bin/sandbox-exec").arg("-p").arg(&policy).args(cmd)` with
`.env_clear().envs(env)`. Add `.stdin/.stdout/.stderr(Stdio::from(fd))`, and when
`controlling_terminal` is set, a `std::os::unix::process::CommandExt::pre_exec` closure doing
`setsid()` then `ioctl(0, TIOCSCTTY, 0)`. `sandbox-exec` execs in place, so the controlling terminal
established before it survives into the harness, and the existing `on_exit` mapping of exit
64/65/71/134 keeps working unchanged.

**Linux** — `crates/isol8-core/src/backends/linux.rs`, `LinuxBackend::spawn` and
`child_setup_and_exec`. In the forked child, **before** `set_no_new_privs()` and
`apply_landlock(&rules)`: `dup2` the three descriptors onto 0/1/2, close the originals, and when
`controlling_terminal` is set, `setsid()` + `ioctl(TIOCSCTTY)`. Then exec as today. Ordering matters
— the descriptors must be in place while the process is still unconfined, and `SandboxChild::forked`
already gives the host a `waitpid`-based handle.

**Windows** — `Error::UnsupportedOs`. ConPTY is a separate piece of work, and
`windows-support.md` already states that Windows enforces no path grants, so a confined pane there
would be documentary anyway.

## 4. Three things a host cannot fix from outside

These are the reason the seam is not just "accept three fds".

**4.1 `TERM` never reaches the child.** `crates/isol8-core/src/env.rs` allowlists
`HOME PATH SHELL TMPDIR USER LOGNAME PWD` and drops everything else, and `Backend::spawn` calls
`env_clear()`. A confined TUI harness with no `TERM` cannot decide what it may draw — it degrades to
unusable, or refuses to start. A caller *can* work around it with `set_env`, but every caller must,
and forgetting produces a blank pane rather than an error.

**Requested:** when `controlling_terminal` is set, pass `TERM` and `COLORTERM` through from the host
environment by default, with `set_env` still overriding. (If you would rather keep the allowlist
untouched, say so and hosts will pass them explicitly — but then it belongs in `integration.md` as a
loud note.)

**4.2 The Seatbelt policy must permit pseudo-tty operations.** `Capability::PseudoTty` exists and
renders as `(allow pseudo-tty)` (`backends/macos.rs`). A pane whose policy omits it fails in a way
that looks like the harness crashing.

**Requested:** `controlling_terminal` implies that capability in the rendered policy, so no host has
to remember. Alternatively state the requirement in the seam's rustdoc and in `integration.md`.

**4.3 Nesting is per-process, and hosts need it once.** `ensure_not_nested()` fails a spawn when
`ISOL8_SANDBOXED` is set. A host that is itself confined can never confine a session, so it should
report that as a capability at startup rather than as a per-pane error — `integration.md` §7 already
says this; the seam's docs should repeat it, because a pty host is exactly where the per-pane error
would be most confusing.

## 5. Tests

- **Unit:** `SandboxStdio::from_tty` dup/ownership semantics — the slave is dup'd three times, the
  original is still owned by the caller, and dropping the struct closes only its own copies.
- **Field test** (`just field-test`, following scenarios 17–19's shape): run a command confined under
  a pty and assert, from inside the sandbox, that `tty` names a pty, that `stty size` matches the
  requested `PtySize`, that a `PtyChild::resize` is observed (the child sees `SIGWINCH` and reports
  the new size), and that a write outside the grants is **still denied** — the seam must not widen
  the policy.
- **Linux specifically:** that the controlling terminal is established before `restrict_self()`, so
  the `TIOCSCTTY` ioctl is not itself denied.

## 6. What the two consumers will call

For the record, so the signature is checked against real use rather than guessed at:

```rust
// agent-manager, confined run: crates/agent-manager/src/{isolate,run}.rs
let confined = isolate::plan(&launch, &spec, &settings)?;      // builds Spec + Context
let mut pty = isol8::sandbox::spawn_pty_in(&confined.spec, &confined.ctx,
                                           PtySize { cols, rows })?;
let reader = pty.try_clone_reader()?;
let writer = pty.take_writer()?;
// ... pump, SIGWINCH -> pty.resize(..), pty.child().wait() -> exit code

// Ubiq, one pane: crates/ubiq-host/src/pty/mod.rs
// same three calls, plus pty.child().kill() when the tab closes.
```

Both want, in this order: **spawn confined on a pty → clone a reader → take a writer → resize →
wait/kill for an exit code.** Nothing else.

## 7. References into `refs/isol8/`

| File | What to look at |
|---|---|
| `crates/isol8-core/src/sandbox.rs` | `Spec`, `Sandbox::{spawn,run,dry_run}`, `SandboxChild` (`process`/`forked` are `pub(crate)`), `ensure_not_nested`, `run_captured` |
| `crates/isol8-core/src/backends/mod.rs` | the `Backend` trait (`spawn`, `output`, `render_policy`), `select()`, `exit_status_from_code` |
| `crates/isol8-core/src/backends/macos.rs` | `MacosBackend::spawn` — `sandbox-exec -p`, `env_clear`, the `on_exit` exit-code mapping; `render_policy`; `Capability::PseudoTty` |
| `crates/isol8-core/src/backends/linux.rs` | `LinuxBackend::spawn` (`fork`), `child_setup_and_exec` (`set_no_new_privs` → `apply_landlock` → `exec`), `render_policy` (`pub(crate)`) |
| `crates/isol8-core/src/env.rs` | `ALLOWLIST`, `build_minimal`, `SANDBOX_MARKER` |
| `crates/isol8-core/src/profile.rs` | `Capability`, `Profile`, `PathGrant` |
| `crates/isol8-core/src/resolve.rs` | `effective_policy_in`, `EffectivePolicy`, `confine_executable`, `spec_from_config` |
| `_docs/integration.md` | §2.1 (the backend is closed by design), §7 (running the session), §8 (known limits — where "no `spawn_in`" is stated) |
| `_docs/embedding.md` | the per-call reference the seam has to be added to |
| `examples/embed_harness.rs` | the worked host integration this seam completes |

Docs to update with the seam: `_docs/embedding.md` (signatures), `_docs/integration.md` (§7, and
drop the pty caveat from §8), `AGENTS.md` (the library-API bullet), and
`_docs/testing-strategies.md` (the new field scenario).
