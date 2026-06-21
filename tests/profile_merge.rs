//! Integration coverage for the deny-first merge + inheritance resolver.
//! Pure logic over the public profile API — no real sandboxing, no exec.

use std::collections::HashMap;

use isol8::profile::{
    merge, resolve_requires, Access, Capability, HomeReplace, MatchKind, Profile,
};

fn grant(path: &str, access: Access) -> isol8::profile::PathGrant {
    isol8::profile::PathGrant {
        path: path.to_string(),
        access,
        r#match: MatchKind::Subpath,
    }
}

fn access_of(p: &Profile, path: &str) -> Option<Access> {
    p.paths.iter().find(|g| g.path == path).map(|g| g.access)
}

#[test]
fn highest_explicit_grant_wins_including_none() {
    let low = Profile {
        paths: vec![grant("/home", Access::Rw)],
        ..Default::default()
    };
    let high = Profile {
        paths: vec![grant("/home", Access::None)],
        ..Default::default()
    };
    let merged = merge(&[low, high]);
    assert_eq!(access_of(&merged, "/home"), Some(Access::None));
}

#[test]
fn top_layer_regrant_overrides_lower_none() {
    let low = Profile {
        paths: vec![grant("/x", Access::None)],
        ..Default::default()
    };
    let high = Profile {
        paths: vec![grant("/x", Access::Rw)],
        ..Default::default()
    };
    let merged = merge(&[low, high]);
    assert_eq!(access_of(&merged, "/x"), Some(Access::Rw));
}

#[test]
fn child_refines_parent() {
    let layer = Profile {
        paths: vec![
            grant("/home", Access::Rw),
            grant("/home/.ssh", Access::None),
        ],
        ..Default::default()
    };
    let merged = merge(&[layer]);
    assert_eq!(access_of(&merged, "/home"), Some(Access::Rw));
    assert_eq!(access_of(&merged, "/home/.ssh"), Some(Access::None));
}

#[test]
fn env_first_writer_wins() {
    let low = Profile {
        env: HashMap::from([("PATH".into(), "/base".into())]),
        ..Default::default()
    };
    let high = Profile {
        env: HashMap::from([("PATH".into(), "/tool".into()), ("X".into(), "1".into())]),
        ..Default::default()
    };
    let merged = merge(&[low, high]);
    assert_eq!(merged.env["PATH"], "/base");
    assert_eq!(merged.env["X"], "1");
}

#[test]
fn home_replace_highest_wins_seed_union() {
    let low = Profile {
        home_replace: Some(HomeReplace {
            enabled: true,
            auto_scratch: true,
            path: None,
            seed: vec!["~/.gitconfig".into()],
        }),
        ..Default::default()
    };
    let high = Profile {
        home_replace: Some(HomeReplace {
            enabled: true,
            auto_scratch: false,
            path: Some("/custom".into()),
            seed: vec!["~/.ssh".into()],
        }),
        ..Default::default()
    };
    let merged = merge(&[low, high]);
    let hr = merged.home_replace.unwrap();
    assert_eq!(hr.path.as_deref(), Some("/custom"));
    assert_eq!(hr.seed.len(), 2);
    assert!(hr.seed.contains(&"~/.gitconfig".to_string()));
    assert!(hr.seed.contains(&"~/.ssh".to_string()));
}

#[test]
fn macos_cap_union_raw_concat() {
    let low = Profile {
        macos: Some(isol8::profile::MacosExtra {
            capabilities: vec![Capability::MachLookup, Capability::Signal],
            raw: "(allow a)".into(),
        }),
        ..Default::default()
    };
    let high = Profile {
        macos: Some(isol8::profile::MacosExtra {
            capabilities: vec![Capability::Signal, Capability::Pasteboard],
            raw: "(allow b)".into(),
        }),
        ..Default::default()
    };
    let merged = merge(&[low, high]);
    let m = merged.macos.unwrap();
    assert_eq!(m.capabilities.len(), 3);
    assert_eq!(m.raw, "(allow a)\n(allow b)\n");
}

#[test]
fn resolve_requires_deps_first_and_diamond_dedup() {
    let all = HashMap::from([
        ("a".to_string(), Profile::default()),
        (
            "b".to_string(),
            Profile {
                requires: vec!["a".into()],
                ..Default::default()
            },
        ),
        (
            "c".to_string(),
            Profile {
                requires: vec!["a".into()],
                ..Default::default()
            },
        ),
        (
            "d".to_string(),
            Profile {
                requires: vec!["b".into(), "c".into()],
                ..Default::default()
            },
        ),
    ]);
    let order = resolve_requires(&["d".into()], &all).unwrap();
    assert_eq!(order.len(), 4); // diamond: a appears once
                                // first resolved layer must be the dependency-free one
    assert_eq!(order[0].0, "a");
    assert!(order[0].1.requires.is_empty());
}

#[test]
fn resolve_requires_cycle_errors_with_path() {
    let all = HashMap::from([
        (
            "a".to_string(),
            Profile {
                requires: vec!["b".into()],
                ..Default::default()
            },
        ),
        (
            "b".to_string(),
            Profile {
                requires: vec!["a".into()],
                ..Default::default()
            },
        ),
    ]);
    let err = resolve_requires(&["a".into()], &all).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("cycle"), "{msg}");
    assert!(msg.contains("a -> b") || msg.contains("b -> a"), "{msg}");
}
