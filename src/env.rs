use std::collections::HashMap;

use crate::profile::Profile;

/// Variables passed through by default (R3.1). Everything else is dropped so host
/// secrets (API keys, tokens) don't leak into the confined process.
const ALLOWLIST: &[&str] = &["HOME", "PATH", "SHELL", "TMPDIR", "USER", "LOGNAME", "PWD"];

/// Build the sanitized environment for the confined process.
///
/// $HOME is resolved FIRST (R4) — `home_override` wins, then any profile-supplied
/// HOME, then the inherited value — so every downstream $HOME-derived grant
/// targets the replacement.
///
/// ponytail: stub. Returns nothing yet; real impl filters std::env by ALLOWLIST,
/// applies the resolved HOME, then merges profile env without override (R3.5).
pub fn build_minimal(_profile: &Profile, _home_override: Option<&str>) -> HashMap<String, String> {
    let _ = ALLOWLIST;
    todo!("filter std::env to ALLOWLIST, resolve HOME first, merge profile env")
}
