//! macOS Seatbelt denial observation for `--analyze` (evolution Phase 6).
//!
//! Default mode scrapes the unified log via `log stream` while the confined
//! process runs. Optional `--author` injects Seatbelt `(trace "…")` (handled in
//! the CLI before spawn).
//!
//! See evo-repo §8.2 and `_docs/wip/multi-evo-plan.md` Phase 6.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crate::analyze::{Denial, DenialAccess};
use crate::error::{Error, Result};

/// Predicate for Sandbox denials in the unified log.
///
/// Broad: any eventMessage containing `deny(` (Seatbelt style). We parse strictly
/// afterward so non-sandbox noise is dropped.
pub const LOG_PREDICATE: &str = "eventMessage CONTAINS \"deny(\"";

/// Parse a single unified-log line (NDJSON or plain text) into a denial, if any.
pub fn parse_log_line(line: &str) -> Option<Denial> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let (msg, pid) = if line.starts_with('{') {
        extract_ndjson_message(line)?
    } else {
        (line.to_string(), 0)
    };
    parse_deny_message(&msg, pid)
}

/// Extract `eventMessage` (and optional `processID`) from a log-stream NDJSON object.
fn extract_ndjson_message(line: &str) -> Option<(String, u32)> {
    // Prefer serde_json for robustness.
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let msg = v
        .get("eventMessage")
        .or_else(|| v.get("message"))
        .and_then(|m| m.as_str())?
        .to_string();
    let pid = v
        .get("processID")
        .or_else(|| v.get("pid"))
        .and_then(|p| p.as_u64())
        .unwrap_or(0) as u32;
    Some((msg, pid))
}

/// Parse a Sandbox denial message body into a [`Denial`].
///
/// Accepts forms such as:
/// - `Sandbox: curl(1234) deny(1) file-read-data /Users/x/.netrc`
/// - `Sandbox: deny(1) file-write-create /private/tmp/foo`
/// - `deny(1) file-read-data /path`
pub fn parse_deny_message(msg: &str, pid_hint: u32) -> Option<Denial> {
    // Must look like a deny.
    let lower = msg.to_ascii_lowercase();
    if !lower.contains("deny") {
        return None;
    }

    // Optional "Sandbox: name(pid) deny…" — only treat (N) as pid when it
    // appears *before* the deny token (not deny(1) itself).
    let mut pid = pid_hint;
    if let Some(rest) = msg.strip_prefix("Sandbox:") {
        let rest = rest.trim_start();
        let deny_at = rest.to_ascii_lowercase().find("deny").unwrap_or(rest.len());
        let before = &rest[..deny_at];
        if let Some(open) = before.rfind('(') {
            if let Some(close) = before[open + 1..].find(')') {
                let maybe_pid = &before[open + 1..open + 1 + close];
                if let Ok(p) = maybe_pid.parse::<u32>() {
                    pid = p;
                }
            }
        }
    }

    // Find deny token, then operation, then path starting with '/'.
    let deny_idx = lower.find("deny")?;
    let after_deny = &msg[deny_idx..];
    // Skip "deny" / "deny(1)" / "deny(1) "
    let after = after_deny
        .strip_prefix("deny")
        .or_else(|| after_deny.strip_prefix("Deny"))?;
    let after = after.trim_start();
    let after = if after.starts_with('(') {
        let end = after.find(')')?;
        after[end + 1..].trim_start()
    } else {
        after
    };

    // Operation is the first whitespace-separated token; path is the rest starting at '/'.
    let path_start = after.find('/')?;
    let op = after[..path_start].trim();
    let path_str = after[path_start..].trim();
    // Path may be followed by more text; take until whitespace if any (paths rarely have spaces).
    let path_str = path_str.split_whitespace().next().unwrap_or(path_str);
    if path_str.is_empty() || !path_str.starts_with('/') {
        return None;
    }

    let access = denial_access_from_op(op);
    Some(Denial {
        path: PathBuf::from(path_str),
        access,
        count: 1,
        pid,
        exe: None,
    })
}

fn denial_access_from_op(op: &str) -> DenialAccess {
    let op = op.to_ascii_lowercase();
    if op.contains("write") || op.contains("create") || op.contains("unlink") {
        DenialAccess::Write
    } else if op.contains("exec") {
        DenialAccess::Exec
    } else if op.contains("metadata") || op.contains("ioctl") {
        DenialAccess::Metadata
    } else if op.contains("read") || op.contains("file") {
        DenialAccess::Read
    } else {
        DenialAccess::Other
    }
}

/// Background `log stream` collector.
pub struct LogStream {
    child: Child,
    rx: Receiver<String>,
    /// Join handle for the reader thread (dropped on stop).
    _reader: Option<thread::JoinHandle<()>>,
}

impl LogStream {
    /// Start `log stream --style ndjson` with the Sandbox deny predicate.
    ///
    /// Waits briefly for the stream process to come up so early denials are not lost.
    pub fn start() -> Result<Self> {
        let mut child = Command::new("log")
            .args([
                "stream",
                "--style",
                "ndjson",
                "--level",
                "debug",
                "--predicate",
                LOG_PREDICATE,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                Error::Message(format!(
                    "failed to start `log stream` (needed for --analyze on macOS): {e}"
                ))
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Message("log stream produced no stdout pipe".into()))?;
        let (tx, rx): (Sender<String>, Receiver<String>) = mpsc::channel();
        let reader = thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(|l| l.ok()) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        // Startup race: give the stream a moment before the confined process runs.
        thread::sleep(Duration::from_millis(400));

        Ok(Self {
            child,
            rx,
            _reader: Some(reader),
        })
    }

    /// Drain collected lines and parse denials.
    ///
    /// When `pid_filter` is `Some`, keep denials whose pid matches **or** is 0
    /// (unknown). If that would drop everything, fall back to all parsed denials
    /// (Seatbelt sometimes logs under kernel/sandbox-exec pid).
    pub fn collect_denials(&self, pid_filter: Option<u32>) -> Vec<Denial> {
        let mut raw = Vec::new();
        while let Ok(line) = self.rx.try_recv() {
            if let Some(d) = parse_log_line(&line) {
                raw.push(d);
            }
        }
        filter_by_pid(raw, pid_filter)
    }

    /// Stop the stream (SIGTERM) and drain remaining lines for `drain_ms`.
    pub fn stop_and_collect(&mut self, pid_filter: Option<u32>, drain_ms: u64) -> Vec<Denial> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        thread::sleep(Duration::from_millis(drain_ms));
        // Reader may still flush; try_recv after wait.
        self.collect_denials(pid_filter)
    }
}

/// Fallback: query recent unified log history (catches denials the live stream missed).
pub fn log_show_recent(seconds: u64, pid_filter: Option<u32>) -> Vec<Denial> {
    let last = format!("{seconds}s");
    let output = Command::new("log")
        .args([
            "show",
            "--style",
            "ndjson",
            "--last",
            &last,
            "--predicate",
            LOG_PREDICATE,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let mut dens = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(d) = parse_log_line(line) {
            dens.push(d);
        }
    }
    filter_by_pid(dens, pid_filter)
}

/// Collect denials around a confined run: start stream → run `spawn_wait` → stop +
/// `log show --last` fallback.
pub fn observe_denials_during<F>(spawn_wait: F) -> Result<(i32, u32, Vec<Denial>)>
where
    F: FnOnce() -> Result<(i32, u32)>,
{
    let mut stream = LogStream::start()?;
    let (code, pid) = spawn_wait()?;
    let mut denials = stream.stop_and_collect(Some(pid), 600);
    if denials.is_empty() {
        // History fallback (stream can miss short-lived processes).
        denials = log_show_recent(15, Some(pid));
    }
    Ok((code, pid, denials))
}

impl Drop for LogStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn filter_by_pid(denials: Vec<Denial>, pid_filter: Option<u32>) -> Vec<Denial> {
    let Some(want) = pid_filter else {
        return denials;
    };
    if want == 0 {
        return denials;
    }
    let filtered: Vec<Denial> = denials
        .iter()
        .filter(|d| d.pid == 0 || d.pid == want)
        .cloned()
        .collect();
    // If filtering removed everything, keep all (better noisy than empty).
    if filtered.is_empty() && !denials.is_empty() {
        denials
    } else {
        filtered
    }
}

/// Seatbelt `(trace "path")` line for `--author` mode (permissive observation).
pub fn seatbelt_trace_directive(path: &std::path::Path) -> String {
    // Escape backslashes and quotes for SBPL string.
    let p = path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("(trace \"{p}\")\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_classic_sandbox_line() {
        let d = parse_deny_message(
            "Sandbox: curl(4242) deny(1) file-read-data /Users/x/.netrc",
            0,
        )
        .unwrap();
        assert_eq!(d.path, PathBuf::from("/Users/x/.netrc"));
        assert_eq!(d.access, DenialAccess::Read);
        assert_eq!(d.pid, 4242);
    }

    #[test]
    fn parse_write_create() {
        let d =
            parse_deny_message("Sandbox: deny(1) file-write-create /private/tmp/foo", 9).unwrap();
        assert_eq!(d.path, PathBuf::from("/private/tmp/foo"));
        assert_eq!(d.access, DenialAccess::Write);
        assert_eq!(d.pid, 9);
    }

    #[test]
    fn parse_ndjson_wrapper() {
        let line = r#"{"eventMessage":"Sandbox: node(99) deny(1) file-read-data /Users/a/.nvm/x","processID":99}"#;
        let d = parse_log_line(line).unwrap();
        assert_eq!(d.path, PathBuf::from("/Users/a/.nvm/x"));
        assert_eq!(d.pid, 99);
        assert_eq!(d.access, DenialAccess::Read);
    }

    #[test]
    fn parse_non_deny_is_none() {
        assert!(parse_deny_message("Sandbox: something allowed", 0).is_none());
        assert!(parse_log_line("hello").is_none());
    }

    #[test]
    fn pid_filter_fallback() {
        let dens = vec![Denial {
            path: PathBuf::from("/a"),
            access: DenialAccess::Read,
            count: 1,
            pid: 1,
            exe: None,
        }];
        // Filter wants 999 — no match → fall back to all.
        let out = filter_by_pid(dens.clone(), Some(999));
        assert_eq!(out.len(), 1);
        let dens2 = vec![Denial {
            path: PathBuf::from("/b"),
            access: DenialAccess::Read,
            count: 1,
            pid: 42,
            exe: None,
        }];
        let out2 = filter_by_pid(dens2, Some(42));
        assert_eq!(out2.len(), 1);
    }

    #[test]
    fn trace_directive_escapes() {
        let s = seatbelt_trace_directive(std::path::Path::new("/tmp/a\"b.sbpl"));
        assert!(s.contains("(trace \""));
        assert!(s.contains("\\\""));
    }
}
