//! Deny-first path policy matching shared by the isol8 engine and the Windows
//! hook DLL. Serialized to JSON and passed to the child via `ISOL8_PATH_POLICY_FILE`.

use serde::{Deserialize, Serialize};

/// Access level for a path grant (mirrors `isol8::profile::Access`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantAccess {
    None,
    Ro,
    Rw,
    Metadata,
}

/// How a grant path is matched (mirrors `isol8::profile::MatchKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantMatch {
    #[default]
    Subpath,
    Literal,
    Prefix,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRule {
    pub path: String,
    pub access: GrantAccess,
    #[serde(default, rename = "match")]
    pub match_kind: GrantMatch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathPolicy {
    pub version: u32,
    pub grants: Vec<PathRule>,
}

impl PathPolicy {
    pub const VERSION: u32 = 1;
    /// Inline JSON policy passed in the child environment (primary transport).
    pub const ENV_VAR: &'static str = "ISOL8_PATH_POLICY";
    /// Optional path to a policy JSON file (fallback when inline env is too large).
    pub const ENV_FILE_VAR: &'static str = "ISOL8_PATH_POLICY_FILE";

    pub fn new(grants: Vec<PathRule>) -> Self {
        Self {
            version: Self::VERSION,
            grants,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Returns `true` if the operation is permitted.
    pub fn allows(&self, raw_path: &str, wants_write: bool) -> bool {
        let path = normalize_path(raw_path);
        if path.is_empty() {
            return false;
        }
        let effective = self.effective_access(&path);
        match effective {
            None => false,
            Some(GrantAccess::None) => false,
            Some(GrantAccess::Rw) => true,
            Some(GrantAccess::Ro) => !wants_write,
            Some(GrantAccess::Metadata) => !wants_write,
        }
    }

    /// Longest matching grant wins (most specific path prefix).
    fn effective_access(&self, path: &str) -> Option<GrantAccess> {
        let mut best: Option<(usize, GrantAccess)> = None;
        for rule in &self.grants {
            if !rule_matches(path, rule) {
                continue;
            }
            let spec = rule.path.len();
            if best.map(|(len, _)| spec > len).unwrap_or(true) {
                best = Some((spec, rule.access));
            }
        }
        best.map(|(_, a)| a)
    }
}

fn normalize_path(path: &str) -> String {
    let mut p = path.replace('/', "\\");
    if p.len() >= 2 && p.as_bytes()[1] == b':' {
        // Drive letter paths: canonicalize case for comparison.
        p = p.to_ascii_lowercase();
    }
    while p.len() > 3 && p.ends_with('\\') {
        p.pop();
    }
    p
}

fn rule_matches(path: &str, rule: &PathRule) -> bool {
    let grant = normalize_path(&rule.path);
    if grant.is_empty() {
        return false;
    }
    match rule.match_kind {
        GrantMatch::Literal => path == grant,
        GrantMatch::Prefix => path.starts_with(&grant),
        GrantMatch::Subpath => {
            if path == grant {
                return true;
            }
            let prefix = if grant.ends_with('\\') {
                grant.clone()
            } else {
                format!("{grant}\\")
            };
            path.starts_with(&prefix)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(grants: Vec<PathRule>) -> PathPolicy {
        PathPolicy::new(grants)
    }

    fn rule(path: &str, access: GrantAccess, m: GrantMatch) -> PathRule {
        PathRule {
            path: path.into(),
            access,
            match_kind: m,
        }
    }

    #[test]
    fn deny_by_default() {
        let p = policy(vec![]);
        assert!(!p.allows(r"C:\outside\secret.txt", false));
    }

    #[test]
    fn rw_subpath_allows_write() {
        let p = policy(vec![rule(
            r"C:\workspace",
            GrantAccess::Rw,
            GrantMatch::Subpath,
        )]);
        assert!(p.allows(r"C:\workspace\out.txt", true));
        assert!(!p.allows(r"C:\outside\x.txt", true));
    }

    #[test]
    fn ro_subpath_denies_write() {
        let p = policy(vec![rule(r"C:\seed", GrantAccess::Ro, GrantMatch::Subpath)]);
        assert!(p.allows(r"C:\seed\data.txt", false));
        assert!(!p.allows(r"C:\seed\new.txt", true));
    }

    #[test]
    fn explicit_none_carves_deny() {
        let p = policy(vec![
            rule(r"C:\home", GrantAccess::Rw, GrantMatch::Subpath),
            rule(r"C:\home\.ssh", GrantAccess::None, GrantMatch::Subpath),
        ]);
        assert!(!p.allows(r"C:\home\.ssh\id_rsa", false));
        assert!(p.allows(r"C:\home\docs\a.txt", true));
    }

    #[test]
    fn json_roundtrip() {
        let p = policy(vec![rule("C:\\tmp", GrantAccess::Rw, GrantMatch::Subpath)]);
        let j = p.to_json().unwrap();
        let back = PathPolicy::from_json(&j).unwrap();
        assert_eq!(back.grants[0].path, "C:\\tmp");
    }
}
