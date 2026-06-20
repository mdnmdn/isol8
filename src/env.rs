use std::collections::HashMap;
use std::path::Path;

use crate::profile::Profile;

/// Variables passed through by default (R3.1). Everything else is dropped so host
/// secrets (API keys, tokens) don't leak into the confined process.
const ALLOWLIST: &[&str] = &["HOME", "PATH", "SHELL", "TMPDIR", "USER", "LOGNAME", "PWD"];

/// Build the sanitized environment for the confined process.
///
/// HOME is authoritative: it is set FIRST from the resolved effective home (R4), so
/// every downstream $HOME-derived grant targets the replacement. Then the host env
/// is filtered down to the allowlist, and finally profile env is folded WITHOUT
/// override (R3.5) — profile values are defaults, not clobbers.
pub fn build_minimal(profile: &Profile, home: &Path) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = HashMap::new();

    // HOME first, authoritative.
    env.insert("HOME".to_string(), home.to_string_lossy().into_owned());

    // Filter the host env to the allowlist (HOME already set, don't overwrite it).
    for (k, v) in std::env::vars() {
        if k == "HOME" {
            continue;
        }
        if ALLOWLIST.contains(&k.as_str()) {
            env.insert(k, v);
        }
    }

    // Profile env: defaults only, no override of anything already present.
    for (k, v) in &profile.env {
        env.entry(k.clone()).or_insert_with(|| v.clone());
    }

    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // These tests mutate process-global env; serialize them to avoid races.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn home_applied_first_and_authoritative() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("HOME", "/real/home");
        let profile = Profile::default();
        let env = build_minimal(&profile, Path::new("/scratch"));
        assert_eq!(env["HOME"], "/scratch"); // effective home wins over host HOME
    }

    #[test]
    fn allowlist_filters_secrets() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SECRET_TOKEN", "shh");
        std::env::set_var("PATH", "/usr/bin");
        let env = build_minimal(&Profile::default(), Path::new("/scratch"));
        assert!(!env.contains_key("SECRET_TOKEN"));
        assert!(env.contains_key("PATH"));
        std::env::remove_var("SECRET_TOKEN");
    }

    #[test]
    fn profile_env_is_default_no_override() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("PATH", "/host/path");
        let profile = Profile {
            env: HashMap::from([
                ("PATH".into(), "/profile/path".into()),
                ("CARGO_TERM_COLOR".into(), "always".into()),
            ]),
            ..Default::default()
        };
        let env = build_minimal(&profile, Path::new("/scratch"));
        // host PATH is allowlisted and set first → profile must not override.
        assert_eq!(env["PATH"], "/host/path");
        // a profile-only var is folded in.
        assert_eq!(env["CARGO_TERM_COLOR"], "always");
    }
}
