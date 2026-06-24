//! Convert merged isol8 profiles into JSON path policy for the hook DLL.

use isol8_path_policy::{GrantAccess, GrantMatch, PathPolicy, PathRule};

use crate::profile::{Access, MatchKind, Profile};

pub fn path_policy_from_profile(profile: &Profile) -> PathPolicy {
    let grants = profile
        .paths
        .iter()
        .map(|g| PathRule {
            path: g.path.clone(),
            access: match g.access {
                Access::None => GrantAccess::None,
                Access::Ro => GrantAccess::Ro,
                Access::Rw => GrantAccess::Rw,
                Access::Metadata => GrantAccess::Metadata,
            },
            match_kind: match g.r#match {
                MatchKind::Subpath => GrantMatch::Subpath,
                MatchKind::Literal => GrantMatch::Literal,
                MatchKind::Prefix | MatchKind::Regex => GrantMatch::Prefix,
            },
        })
        .collect();
    PathPolicy::new(grants)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::PathGrant;

    #[test]
    fn outside_sibling_not_covered_by_root_grant() {
        let root = r"C:\Users\test\AppData\Local\Temp\isol8-ft-1";
        let profile = Profile {
            paths: vec![PathGrant {
                path: root.into(),
                access: Access::Rw,
                r#match: MatchKind::Subpath,
            }],
            ..Default::default()
        };
        let policy = path_policy_from_profile(&profile);
        let outside = r"C:\Users\test\AppData\Local\Temp\outside-isol8-ft-1\secret.txt";
        assert!(!policy.allows(outside, false));
        assert!(!policy.allows(outside, true));
    }

    #[test]
    fn ro_seed_overrides_parent_rw() {
        let root = r"C:\Temp\isol8-ft-1";
        let profile = Profile {
            paths: vec![
                PathGrant {
                    path: root.into(),
                    access: Access::Rw,
                    r#match: MatchKind::Subpath,
                },
                PathGrant {
                    path: format!(r"{root}\seed"),
                    access: Access::Ro,
                    r#match: MatchKind::Subpath,
                },
            ],
            ..Default::default()
        };
        let policy = path_policy_from_profile(&profile);
        assert!(!policy.allows(r"C:\Temp\isol8-ft-1\seed\new.txt", true));
        assert!(policy.allows(r"C:\Temp\isol8-ft-1\seed\data.txt", false));
    }

    #[test]
    fn seed_data_txt_readable_with_ro_grant() {
        let root = r"C:\Temp\isol8-ft-1";
        let seed = format!(r"{root}\seed");
        let profile = Profile {
            paths: vec![
                PathGrant {
                    path: root.into(),
                    access: Access::Rw,
                    r#match: MatchKind::Subpath,
                },
                PathGrant {
                    path: seed.clone(),
                    access: Access::Ro,
                    r#match: MatchKind::Subpath,
                },
            ],
            ..Default::default()
        };
        let policy = path_policy_from_profile(&profile);
        assert!(policy.allows(&format!(r"{seed}\data.txt"), false));
    }

    #[test]
    fn converts_rw_grant() {
        let profile = Profile {
            paths: vec![PathGrant {
                path: r"C:\workspace".into(),
                access: Access::Rw,
                r#match: MatchKind::Subpath,
            }],
            ..Default::default()
        };
        let policy = path_policy_from_profile(&profile);
        assert_eq!(policy.grants.len(), 1);
        assert_eq!(policy.grants[0].access, GrantAccess::Rw);
    }
}
