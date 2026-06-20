use std::collections::HashMap;

use anyhow::{bail, Result};

use super::Backend;
use crate::profile::Profile;

/// Linux backend (primary target).
///
/// Plan (spec R2/R4): build a Landlock ruleset from the path grants
/// (deny-by-default, per-path ro/rw via `LANDLOCK_ACCESS_FS_*`), optionally enter
/// user + mount namespaces to bind the replacement HOME over the real one, set
/// `PR_SET_NO_NEW_PRIVS`, then `execvp` the target. Pure-Landlock is the simple
/// path; the namespace hybrid gives robust HOME replacement (R4.6).
pub struct LinuxBackend;

impl Backend for LinuxBackend {
    fn spawn(&self, _profile: &Profile, _env: &HashMap<String, String>, _cmd: &[String]) -> Result<i32> {
        bail!("linux backend not yet implemented")
    }
}
