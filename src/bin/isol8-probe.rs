//! Minimal file read/write probe for Windows field tests (uses std::fs → CreateFileW).

use std::env;
use std::fs;
use std::io::Write;
use std::process;

fn main() {
    let mut args = env::args().skip(1);
    let op = args.next().unwrap_or_else(|| usage());
    let path = args.next().unwrap_or_else(|| usage());
    let code = match op.as_str() {
        "read" => match fs::read_to_string(&path) {
            Ok(s) => {
                let _ = std::io::stdout().write_all(s.as_bytes());
                0
            }
            Err(e) => {
                eprintln!("isol8-probe read error: {e}");
                1
            }
        },
        "write" => match fs::write(&path, "hi\n") {
            Ok(()) => 0,
            Err(_) => 1,
        },
        _ => usage(),
    };
    process::exit(code);
}

fn usage() -> ! {
    eprintln!("usage: isol8-probe read|write <path>");
    process::exit(2);
}
