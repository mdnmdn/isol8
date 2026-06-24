//! Minimal file read/write probe for Windows field tests (uses std::fs → CreateFileW).
//! The `spawn` op creates a child of this process (grandchild of isol8) to verify hook
//! propagation via `CreateProcess*` detours in `isol8-winhook.dll`.

use std::env;
use std::fs;
use std::io::Write;
use std::process::{self, Command};

fn main() {
    let mut args = env::args().skip(1);
    let op = args.next().unwrap_or_else(|| usage());
    let code = match op.as_str() {
        "read" => {
            let path = args.next().unwrap_or_else(|| usage());
            match fs::read_to_string(&path) {
                Ok(s) => {
                    let _ = std::io::stdout().write_all(s.as_bytes());
                    0
                }
                Err(e) => {
                    eprintln!("isol8-probe read error: {e}");
                    1
                }
            }
        }
        "write" => {
            let path = args.next().unwrap_or_else(|| usage());
            match fs::write(&path, "hi\n") {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
        "spawn" => {
            let child_op = args.next().unwrap_or_else(|| usage());
            let path = args.next().unwrap_or_else(|| usage());
            let exe = env::current_exe().expect("current_exe");
            let status = Command::new(exe)
                .args([child_op.as_str(), path.as_str()])
                .status()
                .unwrap_or_else(|e| {
                    eprintln!("isol8-probe spawn error: {e}");
                    process::exit(1);
                });
            status.code().unwrap_or(1)
        }
        _ => usage(),
    };
    process::exit(code);
}

fn usage() -> ! {
    eprintln!("usage: isol8-probe read|write <path>");
    eprintln!("       isol8-probe spawn read|write <path>");
    process::exit(2);
}
