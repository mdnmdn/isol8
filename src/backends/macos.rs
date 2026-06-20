use std::collections::HashMap;

use anyhow::{bail, Result};

use super::Backend;
use crate::profile::Profile;

/// macOS backend.
///
/// Plan (spec R2): generate a Seatbelt policy string from the path grants
/// (`(deny default)`, `(allow file-read* (subpath ...))` / `(allow file-write* ...)`)
/// and invoke `/usr/bin/sandbox-exec -p <policy>` with the sanitized env.
pub struct MacosBackend;

impl Backend for MacosBackend {
    fn spawn(&self, _profile: &Profile, _env: &HashMap<String, String>, _cmd: &[String]) -> Result<i32> {
        bail!("macos backend not yet implemented")
    }
}
