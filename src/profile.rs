use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::cli::RunArgs;
use crate::home::{self, EffectiveHome};

/// Per-path access level. Default is deny (`None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// Explicit deny — carve a hole out of a broader grant.
    None,
    /// Read-only.
    Ro,
    /// Read-write.
    Rw,
    /// Stat-only, for path resolution without content read (R2.3).
    Metadata,
}

/// How a `PathGrant.path` matches. `subpath` covers a whole subtree (the default);
/// the others mirror Seatbelt matchers and are macOS-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchKind {
    /// Whole subtree beneath `path` (Landlock `PathBeneath` / Seatbelt `subpath`).
    #[default]
    Subpath,
    /// Exact node only.
    Literal,
    /// String prefix.
    Prefix,
    /// Regex match.
    Regex,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathGrant {
    pub path: String,
    pub access: Access,
    #[serde(default, rename = "match")]
    pub r#match: MatchKind,
}

/// macOS-only Seatbelt operation classes with no Linux/Landlock equivalent (§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    MachLookup,
    MachRegister,
    IokitOpen,
    SysctlRead,
    ProcessExec,
    ProcessFork,
    ProcessInfo,
    Signal,
    PseudoTty,
    UserPreferenceRead,
    UserPreferenceWrite,
    IpcPosixShm,
    SysvSem,
    Pasteboard,
}

/// macOS-only capability grants plus raw SBPL passthrough (§8). Applied only by the
/// Seatbelt backend; the Linux backend ignores it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacosExtra {
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Verbatim Seatbelt rules, concatenated after generated rules.
    #[serde(default)]
    pub raw: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeReplace {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub auto_scratch: bool,
    /// Explicit replacement home (overridden by `--home`).
    #[serde(default)]
    pub path: Option<String>,
    /// Real-home entries to seed read-only into the replacement (e.g. "~/.gitconfig").
    #[serde(default)]
    pub seed: Vec<String>,
}

/// One profile layer as authored in TOML/YAML — and also the merged result.
///
/// ponytail: one struct for layer+merged; split if a merged-only field appears.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Names of layers this one depends on; pulled in transitively, deps first.
    /// Accepts `extends` as an alias.
    #[serde(default, alias = "extends")]
    pub requires: Vec<String>,
    #[serde(default)]
    pub paths: Vec<PathGrant>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub home_replace: Option<HomeReplace>,
    #[serde(default)]
    pub macos: Option<MacosExtra>,
}

/// Built-in profile layers, embedded at build time. Real content is authored by the
/// profiles agent; these `include_str!` references just wire them in.
const BUILTIN_PROFILES: &[(&str, &str)] = &[
    ("base", include_str!("../profiles/base.toml")),
    (
        "macos-system",
        include_str!("../profiles/macos-system.toml"),
    ),
];

/// Parse a TOML layer body, attaching the layer name for clear error messages.
fn parse_layer(name: &str, body: &str, source: &str) -> Result<Profile> {
    toml::from_str::<Profile>(body)
        .with_context(|| format!("failed to parse profile layer '{name}' ({source})"))
}

/// Discover user-authored layers under `$XDG_CONFIG_HOME/isol8/profiles` (or
/// `~/.config/isol8/profiles`). Silently skipped if the directory is absent.
fn load_user_layers(map: &mut HashMap<String, Profile>) -> Result<()> {
    let dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(base) if !base.is_empty() => std::path::PathBuf::from(base).join("isol8/profiles"),
        _ => match std::env::var_os("HOME") {
            Some(h) if !h.is_empty() => std::path::PathBuf::from(h).join(".config/isol8/profiles"),
            _ => return Ok(()),
        },
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // absent dir → skip silently
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("reading user profile '{}'", path.display()))?;
        let layer = parse_layer(name, &body, &path.display().to_string())?;
        map.insert(name.to_string(), layer);
    }
    Ok(())
}

/// Build the name→Profile map from embedded builtins plus user layers.
fn load_all_layers() -> Result<HashMap<String, Profile>> {
    let mut map = HashMap::new();
    for (name, body) in BUILTIN_PROFILES {
        let layer = parse_layer(name, body, "built-in")?;
        map.insert((*name).to_string(), layer);
    }
    load_user_layers(&mut map)?;
    Ok(map)
}

/// Expand the selected layers over their transitive `requires` graph, returning
/// them in merge order (dependencies before dependents).
///
/// DFS topo-sort: deps-first, cycle detection (errors with the cycle path), dedup
/// (each layer once). Tie-break by selection / declaration order.
///
/// ponytail: no band field; requires-edges + selection order suffice.
pub fn resolve_requires(
    selected: &[String],
    all: &HashMap<String, Profile>,
) -> Result<Vec<Profile>> {
    // States: not visited, on the current DFS stack (gray), or done (black).
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Gray,
        Black,
    }
    let mut state: HashMap<String, State> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    fn visit(
        name: &str,
        all: &HashMap<String, Profile>,
        state: &mut HashMap<String, State>,
        order: &mut Vec<String>,
        stack: &mut Vec<String>,
    ) -> Result<()> {
        match state.get(name) {
            Some(State::Black) => return Ok(()), // diamond dedup
            Some(State::Gray) => {
                let mut cycle = stack.clone();
                cycle.push(name.to_string());
                let from = cycle.iter().position(|n| n == name).unwrap_or(0);
                let path = cycle[from..].join(" -> ");
                bail!("profile dependency cycle detected: {path}");
            }
            None => {}
        }
        let layer = all
            .get(name)
            .ok_or_else(|| anyhow!("unknown profile layer '{name}' referenced via requires"))?;
        state.insert(name.to_string(), State::Gray);
        stack.push(name.to_string());
        for dep in &layer.requires {
            visit(dep, all, state, order, stack)?;
        }
        stack.pop();
        state.insert(name.to_string(), State::Black);
        order.push(name.to_string());
        Ok(())
    }

    let mut stack: Vec<String> = Vec::new();
    for name in selected {
        if !all.contains_key(name) {
            bail!("unknown profile '{name}' (not a built-in or user layer)");
        }
        visit(name, all, &mut state, &mut order, &mut stack)?;
    }

    Ok(order.into_iter().map(|n| all[&n].clone()).collect())
}

/// Merge layers deny-first into one effective profile (additive, §6).
///
/// - paths: keyed by `(path, match)`; the highest (most-recent) layer with an
///   explicit grant wins, including `none`. A child path is its own key.
/// - env: union, first-writer-wins (lower layers are defaults).
/// - home_replace: from the highest layer that sets it; seed lists unioned.
/// - macos: capabilities unioned (deduped); raw blocks concatenated in layer order.
pub fn merge(layers: &[Profile]) -> Profile {
    let mut env: HashMap<String, String> = HashMap::new();

    // (path, match) -> (order_index, grant). Higher index wins.
    let mut path_map: HashMap<(String, MatchKind), (usize, PathGrant)> = HashMap::new();
    let mut path_order: Vec<(String, MatchKind)> = Vec::new();

    let mut home_replace: Option<HomeReplace> = None;
    let mut seed: Vec<String> = Vec::new();

    let mut caps: Vec<Capability> = Vec::new();
    let mut raw = String::new();

    for (idx, layer) in layers.iter().enumerate() {
        // env: first writer wins.
        for (k, v) in &layer.env {
            env.entry(k.clone()).or_insert_with(|| v.clone());
        }

        // paths: highest layer with an explicit grant wins.
        for grant in &layer.paths {
            let key = (grant.path.clone(), grant.r#match);
            if !path_map.contains_key(&key) {
                path_order.push(key.clone());
            }
            path_map.insert(key, (idx, grant.clone()));
        }

        // home_replace: highest layer that sets it; union seeds.
        if let Some(hr) = &layer.home_replace {
            for s in &hr.seed {
                if !seed.contains(s) {
                    seed.push(s.clone());
                }
            }
            home_replace = Some(hr.clone());
        }

        // macos: union caps, concat raw.
        if let Some(m) = &layer.macos {
            for c in &m.capabilities {
                if !caps.contains(c) {
                    caps.push(*c);
                }
            }
            if !m.raw.is_empty() {
                raw.push_str(&m.raw);
                if !m.raw.ends_with('\n') {
                    raw.push('\n');
                }
            }
        }
    }

    let paths: Vec<PathGrant> = path_order
        .into_iter()
        .map(|key| path_map[&key].1.clone())
        .collect();

    let home_replace = home_replace.map(|mut hr| {
        hr.seed = seed;
        hr
    });

    let macos = if caps.is_empty() && raw.is_empty() {
        None
    } else {
        Some(MacosExtra {
            capabilities: caps,
            raw,
        })
    };

    Profile {
        requires: Vec::new(),
        paths,
        env,
        home_replace,
        macos,
    }
}

/// Build the top invocation-override layer from `--add-dirs-rw` / `--add-dirs-ro`.
fn overrides_layer(run: &RunArgs) -> Profile {
    let mut paths = Vec::new();
    for dir in &run.add_dirs_rw {
        paths.push(PathGrant {
            path: dir.clone(),
            access: Access::Rw,
            r#match: MatchKind::Subpath,
        });
    }
    for dir in &run.add_dirs_ro {
        paths.push(PathGrant {
            path: dir.clone(),
            access: Access::Ro,
            r#match: MatchKind::Subpath,
        });
    }
    Profile {
        paths,
        ..Default::default()
    }
}

/// Resolve the selected layers over their inheritance graph (deps-first), without
/// merging or `~`-expansion. Used by `home::resolve` to find the effective
/// `home_replace` before path grants are computed (R4.2).
pub fn resolved_layers(run: &RunArgs) -> Result<Vec<Profile>> {
    let all = load_all_layers()?;
    resolve_requires(&run.profiles, &all)
}

/// Load profile layers named on the CLI, resolve inheritance, expand `~` against the
/// effective home, merge deny-first, then fold invocation overrides as the top layer.
///
/// `home` must be resolved *before* this runs so `~` expands against the effective
/// home (R4.2). See _docs/profile-model.md §7.
pub fn load(run: &RunArgs, home: &EffectiveHome) -> Result<Profile> {
    let resolved = resolved_layers(run)?;

    // Expand `~` in every grant against the effective home before merge.
    let mut layers: Vec<Profile> = resolved
        .into_iter()
        .map(|mut layer| {
            for grant in &mut layer.paths {
                grant.path = home::expand_tilde(&grant.path, &home.path);
            }
            layer
        })
        .collect();

    // Invocation overrides enter as the top (highest-priority) layer.
    let mut over = overrides_layer(run);
    for grant in &mut over.paths {
        grant.path = home::expand_tilde(&grant.path, &home.path);
    }
    layers.push(over);

    Ok(merge(&layers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(path: &str, access: Access) -> PathGrant {
        PathGrant {
            path: path.to_string(),
            access,
            r#match: MatchKind::Subpath,
        }
    }

    fn find<'a>(p: &'a Profile, path: &str) -> Option<&'a PathGrant> {
        p.paths.iter().find(|g| g.path == path)
    }

    #[test]
    fn merge_highest_explicit_wins_including_none() {
        let low = Profile {
            paths: vec![grant("/a", Access::Rw)],
            ..Default::default()
        };
        let high = Profile {
            paths: vec![grant("/a", Access::None)],
            ..Default::default()
        };
        let merged = merge(&[low, high]);
        assert_eq!(find(&merged, "/a").unwrap().access, Access::None);

        // Re-grant in a still-higher layer overrides a lower `none`.
        let low = Profile {
            paths: vec![grant("/a", Access::None)],
            ..Default::default()
        };
        let high = Profile {
            paths: vec![grant("/a", Access::Rw)],
            ..Default::default()
        };
        let merged = merge(&[low, high]);
        assert_eq!(find(&merged, "/a").unwrap().access, Access::Rw);
    }

    #[test]
    fn merge_child_refines_parent() {
        let layer = Profile {
            paths: vec![
                grant("/home", Access::Rw),
                grant("/home/.ssh", Access::None),
            ],
            ..Default::default()
        };
        let merged = merge(&[layer]);
        assert_eq!(find(&merged, "/home").unwrap().access, Access::Rw);
        assert_eq!(find(&merged, "/home/.ssh").unwrap().access, Access::None);
    }

    #[test]
    fn merge_distinct_match_kinds_are_separate_keys() {
        let layer = Profile {
            paths: vec![
                PathGrant {
                    path: "/a".into(),
                    access: Access::Rw,
                    r#match: MatchKind::Subpath,
                },
                PathGrant {
                    path: "/a".into(),
                    access: Access::Ro,
                    r#match: MatchKind::Literal,
                },
            ],
            ..Default::default()
        };
        let merged = merge(&[layer]);
        assert_eq!(merged.paths.len(), 2);
    }

    #[test]
    fn merge_env_first_writer_wins() {
        let low = Profile {
            env: HashMap::from([("PATH".into(), "/base".into())]),
            ..Default::default()
        };
        let high = Profile {
            env: HashMap::from([
                ("PATH".into(), "/tool".into()),
                ("EXTRA".into(), "1".into()),
            ]),
            ..Default::default()
        };
        let merged = merge(&[low, high]);
        assert_eq!(merged.env["PATH"], "/base");
        assert_eq!(merged.env["EXTRA"], "1");
    }

    #[test]
    fn merge_home_replace_highest_wins_seed_union() {
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
        assert_eq!(hr.path.as_deref(), Some("/custom")); // highest wins
        assert!(!hr.auto_scratch);
        assert!(hr.seed.contains(&"~/.gitconfig".to_string()));
        assert!(hr.seed.contains(&"~/.ssh".to_string()));
        assert_eq!(hr.seed.len(), 2);
    }

    #[test]
    fn merge_macos_cap_union_raw_concat() {
        let low = Profile {
            macos: Some(MacosExtra {
                capabilities: vec![Capability::MachLookup, Capability::Signal],
                raw: "(allow a)".into(),
            }),
            ..Default::default()
        };
        let high = Profile {
            macos: Some(MacosExtra {
                capabilities: vec![Capability::Signal, Capability::Pasteboard],
                raw: "(allow b)".into(),
            }),
            ..Default::default()
        };
        let merged = merge(&[low, high]);
        let m = merged.macos.unwrap();
        assert_eq!(m.capabilities.len(), 3); // Signal deduped
        assert!(m.capabilities.contains(&Capability::MachLookup));
        assert!(m.capabilities.contains(&Capability::Pasteboard));
        assert_eq!(m.raw, "(allow a)\n(allow b)\n"); // layer order
    }

    #[test]
    fn resolve_requires_deps_first() {
        let all = HashMap::from([
            (
                "base".to_string(),
                Profile {
                    ..Default::default()
                },
            ),
            (
                "rust".to_string(),
                Profile {
                    requires: vec!["base".into()],
                    ..Default::default()
                },
            ),
        ]);
        let order = resolve_requires(&["rust".into()], &all).unwrap();
        // base must precede rust; both present.
        assert_eq!(order.len(), 2);
        assert!(order[0].requires.is_empty()); // base first
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
        assert!(msg.contains("cycle"), "msg: {msg}");
        assert!(msg.contains("a") && msg.contains("b"), "msg: {msg}");
    }

    #[test]
    fn resolve_requires_diamond_dedup() {
        // d -> b -> a ; d -> c -> a  => a once, then b/c, then d.
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
        assert_eq!(order.len(), 4); // a appears once
    }

    #[test]
    fn resolve_requires_unknown_layer_errors() {
        let all = HashMap::from([("a".to_string(), Profile::default())]);
        let err = resolve_requires(&["nope".into()], &all).unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn deny_unknown_fields_rejects_typo() {
        let err = toml::from_str::<Profile>("pathz = []").unwrap_err();
        assert!(err.to_string().contains("pathz") || err.to_string().contains("unknown"));
    }
}
