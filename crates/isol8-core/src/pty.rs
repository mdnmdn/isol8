//! Pseudo-terminal seam for hosts (unix only).
//!
//! An interactive agent harness is not a filter over stdout: it drives a full
//! screen (alternate screen, absolute cursor addressing, raw keystrokes, resize
//! redraw), so it needs a **controlling terminal** and its geometry has to reach
//! the kernel so the process is signalled on resize.
//!
//! This module provides the two pieces a host needs:
//!
//! - [`SandboxStdio`] — the primitive: three descriptors the confined child's
//!   standard streams are wired to, plus whether `stdin` becomes its controlling
//!   terminal. A host that already owns a pty pair hands over the slave.
//! - [`PtyChild`] — the convenience: [`crate::sandbox::Sandbox::spawn_pty`] opens
//!   the pty itself and returns the confined child together with the master side,
//!   with the three calls a host actually makes ([`PtyChild::try_clone_reader`],
//!   [`PtyChild::take_writer`], [`PtyChild::resize`]).
//!
//! Both paths keep the resolve pipeline verbatim — `ensure_not_nested`,
//! `effective_policy`, `home::materialize`, `confine_executable` — and differ only
//! in how the backend is asked to spawn. **The seam never widens the policy**, with
//! one deliberate exception documented on
//! [`Sandbox::spawn_with_stdio`](crate::sandbox::Sandbox::spawn_with_stdio): a
//! controlling terminal implies the macOS `pseudo-tty` capability and passes `TERM`
//! / `COLORTERM` through, because a pane that lacks either fails in a way that
//! looks like the harness crashing rather than like a policy denial.
//!
//! There is **no shim process**: macOS `sandbox-exec` `execve`s in place and Linux
//! forks exactly once, so there is one process per pty — a pid the host can `kill`
//! and `wait` on directly, exactly like an unconfined pane.
//!
//! ## Nesting
//!
//! `ensure_not_nested` fails a spawn when `ISOL8_SANDBOXED` is set: a host that is
//! itself confined can never confine a session. Probe that **once at startup** and
//! report it as a capability, not per pane — a per-pane [`crate::Error::NestedSandbox`]
//! is exactly the confusing failure a pty host should avoid.
//!
//! Windows (ConPTY) is not supported; the seam is `cfg(unix)`.

use std::fs::File;
#[cfg(target_os = "linux")]
use std::os::fd::IntoRawFd;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use crate::error::{Error, Result};
use crate::sandbox::SandboxChild;

/// Where a confined child's standard streams come from.
///
/// Construct with [`SandboxStdio::from_tty`] (one tty slave for all three streams,
/// with a controlling terminal) or [`SandboxStdio::from_fds`] (three explicit
/// descriptors, no controlling terminal). The struct owns its descriptors and
/// closes them on drop.
///
/// `#[non_exhaustive]`: use the constructors rather than a struct literal.
#[non_exhaustive]
pub struct SandboxStdio {
    /// Descriptor wired to the child's fd 0.
    pub stdin: OwnedFd,
    /// Descriptor wired to the child's fd 1.
    pub stdout: OwnedFd,
    /// Descriptor wired to the child's fd 2.
    pub stderr: OwnedFd,
    /// When true the child calls `setsid()` then `ioctl(0, TIOCSCTTY, 0)` before
    /// exec, so `stdin` becomes its controlling terminal.
    pub controlling_terminal: bool,
}

impl std::fmt::Debug for SandboxStdio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxStdio")
            .field("stdin", &self.stdin.as_raw_fd())
            .field("stdout", &self.stdout.as_raw_fd())
            .field("stderr", &self.stderr.as_raw_fd())
            .field("controlling_terminal", &self.controlling_terminal)
            .finish()
    }
}

impl SandboxStdio {
    /// The three streams from one tty slave, with `controlling_terminal = true`.
    ///
    /// `slave` is **consumed**: it is `dup`'d three times (one descriptor per
    /// stream) and the original is closed. A caller that needs to keep the slave
    /// open should pass `slave.try_clone()?`. Dropping the returned struct closes
    /// only its own three copies — which is what makes the pty master see EOF once
    /// the confined child has exited.
    pub fn from_tty(slave: OwnedFd) -> Result<Self> {
        let stdin = slave.try_clone().map_err(dup_err)?;
        let stdout = slave.try_clone().map_err(dup_err)?;
        let stderr = slave.try_clone().map_err(dup_err)?;
        drop(slave);
        Ok(Self {
            stdin,
            stdout,
            stderr,
            controlling_terminal: true,
        })
    }

    /// Three explicit descriptors (pipes, files, sockets, …), no controlling
    /// terminal. Use this for a host that pumps bytes but does not need a tty.
    pub fn from_fds(stdin: OwnedFd, stdout: OwnedFd, stderr: OwnedFd) -> Self {
        Self {
            stdin,
            stdout,
            stderr,
            controlling_terminal: false,
        }
    }

    /// Install these descriptors as fd 0/1/2 of the **current** process and, when
    /// requested, make fd 0 the controlling terminal.
    ///
    /// Called by the Linux backend in the forked child *before* `no_new_privs` and
    /// Landlock. Ordering matters: the descriptors and the `TIOCSCTTY` ioctl must
    /// land while the process is still unconfined, or the ioctl is itself denied.
    /// (macOS does not need this — `std::process::Command` installs the descriptors
    /// itself and only the `TIOCSCTTY` part runs from a `pre_exec` closure.)
    ///
    /// # Safety / async-signal-safety
    ///
    /// Post-`fork` / pre-`exec` code: only `dup2`, `close`, `setsid` and `ioctl`
    /// are called, all async-signal-safe. No allocation on the success path.
    #[cfg(target_os = "linux")]
    pub(crate) fn apply_to_current_process(self) -> Result<()> {
        let ctty = self.controlling_terminal;
        let fds: [RawFd; 3] = [
            self.stdin.into_raw_fd(),
            self.stdout.into_raw_fd(),
            self.stderr.into_raw_fd(),
        ];

        for (target, &fd) in fds.iter().enumerate() {
            let target = target as RawFd;
            if fd != target && unsafe { libc::dup2(fd, target) } < 0 {
                return Err(Error::Message(format!(
                    "dup2({fd} -> {target}) failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }
        // Close the originals now that 0/1/2 point at them. Never close a
        // descriptor that already *was* 0/1/2 — it is the live stream.
        for &fd in &fds {
            if fd > 2 {
                unsafe { libc::close(fd) };
            }
        }

        if ctty {
            set_controlling_terminal()?;
        }
        Ok(())
    }
}

/// `setsid()` + `ioctl(0, TIOCSCTTY, 0)` on the current process.
///
/// A new session is required before a tty can be claimed; `setsid` fails with
/// `EPERM` when the process is already a group leader, which is harmless here (the
/// `TIOCSCTTY` below is the part that must succeed).
pub(crate) fn set_controlling_terminal() -> Result<()> {
    unsafe {
        libc::setsid();
        if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
            return Err(Error::Message(format!(
                "ioctl(TIOCSCTTY) failed: {}",
                std::io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

/// Terminal geometry in character cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtySize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

impl Default for PtySize {
    /// The conventional 80x24.
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

impl PtySize {
    fn to_winsize(self) -> libc::winsize {
        libc::winsize {
            ws_row: self.rows,
            ws_col: self.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
}

/// Open a pseudo-terminal pair sized to `size`, returning `(master, slave)`.
///
/// Exposed so a host can drive [`crate::backends::Backend::spawn_with_stdio`]
/// directly and still keep the master; [`crate::sandbox::Sandbox::spawn_pty`] is
/// the usual entry point and wraps this.
pub fn open_pty(size: PtySize) -> Result<(OwnedFd, OwnedFd)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    // The `winsize` argument is `*const` on Linux and `*mut` on the BSDs/macOS, so
    // pass NULL and set the geometry with TIOCSWINSZ below — same result, no cfg.
    // Nothing observes the pty between the two calls: the child is not spawned yet.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc < 0 {
        return Err(Error::Message(format!(
            "openpty failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: openpty returned success, so both are fresh owned descriptors.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    set_winsize(master.as_raw_fd(), size)?;
    Ok((master, slave))
}

/// `TIOCSWINSZ` on a pty master.
fn set_winsize(master: RawFd, size: PtySize) -> Result<()> {
    let ws = size.to_winsize();
    if unsafe { libc::ioctl(master, libc::TIOCSWINSZ as _, &ws) } < 0 {
        return Err(Error::Message(format!(
            "ioctl(TIOCSWINSZ) failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// A confined child plus the master side of the terminal it runs on.
///
/// Dropping this closes the master, which sends `SIGHUP` to the session on the
/// slave side; it does **not** wait for or kill the child. A host that owns the
/// pane lifecycle should [`kill`](SandboxChild::kill) and
/// [`wait`](SandboxChild::wait) through [`PtyChild::child`] explicitly.
pub struct PtyChild {
    child: SandboxChild,
    master: OwnedFd,
}

impl PtyChild {
    /// Pair an already-spawned [`SandboxChild`] with the pty master it runs on.
    ///
    /// For a host that called [`open_pty`] plus
    /// [`Backend::spawn_with_stdio`](crate::backends::Backend::spawn_with_stdio)
    /// itself and wants the [`PtyChild`] conveniences anyway.
    pub fn from_parts(child: SandboxChild, master: OwnedFd) -> Self {
        Self { child, master }
    }

    /// The confined child — `id`, `wait`, `kill`.
    pub fn child(&mut self) -> &mut SandboxChild {
        &mut self.child
    }

    /// The pty master, borrowed.
    pub fn master(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    /// Split into the child handle and the pty master.
    pub fn into_parts(self) -> (SandboxChild, OwnedFd) {
        (self.child, self.master)
    }

    /// Set the terminal geometry (`TIOCSWINSZ` on the master).
    ///
    /// The kernel signals `SIGWINCH` to the confined session, so the harness
    /// redraws at the new size. Call this from the host's own `SIGWINCH` handling.
    pub fn resize(&self, size: PtySize) -> Result<()> {
        set_winsize(self.master.as_raw_fd(), size)
    }

    /// Read the current terminal geometry (`TIOCGWINSZ` on the master).
    pub fn get_size(&self) -> Result<PtySize> {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCGWINSZ as _, &mut ws) } < 0 {
            return Err(Error::Message(format!(
                "ioctl(TIOCGWINSZ) failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(PtySize {
            cols: ws.ws_col,
            rows: ws.ws_row,
        })
    }

    /// A `dup` of the master as a [`File`], for the host's reader thread.
    ///
    /// Repeatable — each call returns an independent descriptor. Reads return EOF
    /// (or `EIO` on Linux) once the confined session has exited and every slave
    /// copy is closed.
    pub fn try_clone_reader(&self) -> Result<File> {
        self.dup_master()
    }

    /// A `dup` of the master as a [`File`], for writing keystrokes.
    ///
    /// Takes `&mut self` to mirror the shape of `portable-pty`'s `MasterPty`, so a
    /// host can keep one internal abstraction over the confined and unconfined
    /// paths; the descriptor is a dup either way.
    pub fn take_writer(&mut self) -> Result<File> {
        self.dup_master()
    }

    fn dup_master(&self) -> Result<File> {
        let fd = self.master.try_clone().map_err(dup_err)?;
        Ok(File::from(fd))
    }
}

impl std::fmt::Debug for PtyChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyChild")
            .field("pid", &self.child.id())
            .field("master", &self.master.as_raw_fd())
            .finish()
    }
}

fn dup_err(e: std::io::Error) -> Error {
    Error::Message(format!("duplicating a descriptor failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fd_is_open(fd: RawFd) -> bool {
        unsafe { libc::fcntl(fd, libc::F_GETFD) != -1 }
    }

    #[test]
    fn open_pty_honours_requested_size() {
        let (master, slave) = open_pty(PtySize {
            cols: 100,
            rows: 42,
        })
        .unwrap();
        let child = SandboxChild::exited(0);
        let pty = PtyChild::from_parts(child, master);
        assert_eq!(
            pty.get_size().unwrap(),
            PtySize {
                cols: 100,
                rows: 42
            }
        );
        pty.resize(PtySize {
            cols: 120,
            rows: 30,
        })
        .unwrap();
        assert_eq!(
            pty.get_size().unwrap(),
            PtySize {
                cols: 120,
                rows: 30
            }
        );
        drop(slave);
    }

    // from_tty dups the slave three times and closes the original; dropping the
    // struct closes only its own copies, so a slave the caller cloned first stays
    // usable (this is what lets a host keep the slave for a second child).
    #[test]
    fn from_tty_dups_three_times_and_owns_only_its_copies() {
        let (_master, slave) = open_pty(PtySize::default()).unwrap();
        let kept = slave.try_clone().unwrap();
        let kept_raw = kept.as_raw_fd();
        let original_raw = slave.as_raw_fd();

        let stdio = SandboxStdio::from_tty(slave).unwrap();
        let raws = [
            stdio.stdin.as_raw_fd(),
            stdio.stdout.as_raw_fd(),
            stdio.stderr.as_raw_fd(),
        ];
        assert!(stdio.controlling_terminal);
        // three distinct, live descriptors, none of them the consumed original
        assert_ne!(raws[0], raws[1]);
        assert_ne!(raws[1], raws[2]);
        assert_ne!(raws[0], raws[2]);
        for r in raws {
            assert!(fd_is_open(r), "fd {r} should be open");
            assert_ne!(r, original_raw, "the consumed slave must not be reused");
        }

        drop(stdio);
        for r in raws {
            assert!(!fd_is_open(r), "fd {r} should be closed with the struct");
        }
        // the caller's own clone survived
        assert!(fd_is_open(kept_raw));
        drop(kept);
    }

    // Ground truth for the primitives, independent of any backend: a child whose
    // stdio comes from `from_tty` sees a real controlling terminal at the requested
    // geometry, and a `PtyChild::resize` reaches the kernel. This mirrors what the
    // macOS backend does around `sandbox-exec` (descriptors + a `pre_exec` ctty);
    // the confined equivalents are field scenarios 20–22.
    #[test]
    fn child_on_the_seam_gets_a_ctty_at_the_requested_size() {
        use std::io::Read;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let (master, slave) = open_pty(PtySize {
            cols: 100,
            rows: 42,
        })
        .unwrap();
        let stdio = SandboxStdio::from_tty(slave).unwrap();

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("tty; stty size");
        // Other unit tests mutate the process-global PATH; pin one for the child.
        cmd.env("PATH", "/usr/bin:/bin");
        cmd.stdin(Stdio::from(stdio.stdin))
            .stdout(Stdio::from(stdio.stdout))
            .stderr(Stdio::from(stdio.stderr));
        unsafe {
            cmd.pre_exec(|| {
                set_controlling_terminal().map_err(|e| std::io::Error::other(e.to_string()))
            });
        }
        let child = cmd.spawn().expect("spawn /bin/sh on the pty");

        let pty = PtyChild::from_parts(SandboxChild::exited(0), master);
        let mut reader = pty.try_clone_reader().unwrap();

        // Read until the child exits; then drop every master reference so the
        // remaining read returns EOF (or EIO on Linux) instead of blocking.
        let mut out = Vec::new();
        let mut buf = [0u8; 512];
        let mut child = child;
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    // `tty` + `stty size` are two lines; stop once both arrived
                    // (EOF below is the normal exit, this is just belt and braces).
                    if out.iter().filter(|b| **b == b'\n').count() >= 2 {
                        break;
                    }
                }
            }
        }
        let _ = child.wait();
        let text = String::from_utf8_lossy(&out).replace('\r', "");

        assert!(
            text.contains("/dev/ttys") || text.contains("/dev/pts/"),
            "fd 0 should be a pty; got: {text:?}"
        );
        assert!(
            text.contains("42 100"),
            "stty should report the requested 42x100; got: {text:?}"
        );

        // TIOCSWINSZ on the master is observable through TIOCGWINSZ.
        pty.resize(PtySize {
            cols: 132,
            rows: 50,
        })
        .unwrap();
        assert_eq!(
            pty.get_size().unwrap(),
            PtySize {
                cols: 132,
                rows: 50
            }
        );
    }

    #[test]
    fn from_fds_does_not_request_a_controlling_terminal() {
        let (master, slave) = open_pty(PtySize::default()).unwrap();
        let a = slave.try_clone().unwrap();
        let b = slave.try_clone().unwrap();
        let stdio = SandboxStdio::from_fds(slave, a, b);
        assert!(!stdio.controlling_terminal);
        drop(stdio);
        drop(master);
    }
}
