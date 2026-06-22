use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, ResultExt};
use crate::filter::{self, RunContext};
use crate::home::{self, EffectiveHome};
use crate::sandbox::Spec;

/// Per-path access level. Default is deny (`None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize, Serialize)]
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

/// A single path rule: what path, what access level, and how the path is matched.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathGrant {
    /// Filesystem path subject to this grant (may contain `~` or `#HOME` tokens).
    pub path: String,
    /// Access level to grant (or deny) for this path.
    pub access: Access,
    /// How `path` is interpreted against the filesystem (default: `subpath`).
    #[serde(default, rename = "match")]
    pub r#match: MatchKind,
}

/// macOS-only Seatbelt operation classes with no Linux/Landlock equivalent (§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Allow Mach service name lookups via the bootstrap server.
    MachLookup,
    /// Allow registration of Mach service names.
    MachRegister,
    /// Allow opening IOKit user-client connections.
    IokitOpen,
    /// Allow reading sysctl values.
    SysctlRead,
    /// Allow executing other processes.
    ProcessExec,
    /// Allow forking child processes.
    ProcessFork,
    /// Allow querying process info (e.g. `sysctl proc_info`).
    ProcessInfo,
    /// Allow sending signals to other processes.
    Signal,
    /// Allow allocating a pseudo-terminal device.
    PseudoTty,
    /// Allow reading `CFPreferences` / `NSUserDefaults`.
    UserPreferenceRead,
    /// Allow writing `CFPreferences` / `NSUserDefaults`.
    UserPreferenceWrite,
    /// Allow POSIX shared-memory operations.
    IpcPosixShm,
    /// Allow System V semaphore operations.
    SysvSem,
    /// Allow access to the pasteboard (clipboard).
    Pasteboard,
}

/// Windows AppContainer capabilities, mapped to well-known capability SIDs (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowsCapability {
    /// Outbound internet access (TCP/UDP client).
    InternetClient,
    /// Bidirectional internet access (client + server).
    InternetClientServer,
    /// Access to private/intranet network resources.
    PrivateNetworkClientServer,
    /// Access to the Documents library.
    DocumentsLibrary,
    /// Access to the Pictures library.
    PicturesLibrary,
    /// Access to the Videos library.
    VideosLibrary,
    /// Access to the Music library.
    MusicLibrary,
    /// Use of Windows integrated authentication (Kerberos/NTLM).
    EnterpriseAuthentication,
    /// Access to shared user certificate stores.
    SharedUserCertificates,
    /// Access to removable storage devices.
    RemovableStorage,
    /// Access to the Appointments (calendar) store.
    Appointments,
    /// Access to the Contacts store.
    Contacts,
}

/// macOS-only capability grants plus raw SBPL passthrough (§8). Applied only by the
/// Seatbelt backend; the Linux backend ignores it.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MacosExtra {
    /// Seatbelt operation classes to allow beyond filesystem grants.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Verbatim Seatbelt rules, concatenated after generated rules.
    #[serde(default)]
    pub raw: String,
}

/// Windows AppContainer capability grants. Applied only by the Windows backend.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowsExtra {
    /// AppContainer capability SIDs to grant the sandboxed process.
    #[serde(default)]
    pub capabilities: Vec<WindowsCapability>,
}

/// Conditional policy bundle within a layer (filter + grants).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Run-context filter that gates this policy block (empty = always applies).
    #[serde(default)]
    pub filter: ProfileFilter,
    /// Path grants applied when this policy's filter matches.
    #[serde(default)]
    pub paths: Vec<PathGrant>,
    /// macOS-specific capability grants applied when this policy's filter matches.
    #[serde(default)]
    pub macos: Option<MacosExtra>,
    /// Windows-specific capability grants applied when this policy's filter matches.
    #[serde(default)]
    pub windows: Option<WindowsExtra>,
}

/// Optional layer/policy selector for auto-profile resolution (OS, arch, executable).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileFilter {
    /// Operating system names that activate this layer/policy (e.g. `["linux", "macos"]`).
    #[serde(default)]
    pub os: Vec<String>,
    /// CPU architectures that activate this layer/policy (e.g. `["x86_64", "aarch64"]`).
    #[serde(default)]
    pub arch: Vec<String>,
    /// Command basename patterns that trigger auto-selection of this layer.
    #[serde(default)]
    pub executables: Vec<String>,
}

/// HOME substitution configuration: activate, path, and optional seed entries (R4).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HomeReplace {
    /// Whether home replacement is active for this layer.
    #[serde(default)]
    pub enabled: bool,
    /// Create a temporary scratch directory as the replacement home automatically.
    #[serde(default)]
    pub auto_scratch: bool,
    /// Explicit replacement home (overridden by `--home`).
    #[serde(default)]
    pub path: Option<String>,
    /// Real-home entries to seed read-only into the replacement (e.g. "~/.gitconfig").
    #[serde(default)]
    pub seed: Vec<String>,
}

/// Command rewrite: ensure certain arguments are present in the confined command.
///
/// Gated by the layer's `filter` (e.g. `executables = ["claude"]`), so it only
/// applies to matching commands. Missing args are inserted right after `argv[0]`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rewrite {
    /// Arguments that must be present; each is inserted after `argv[0]` if absent.
    #[serde(default)]
    pub ensure_args: Vec<String>,
}

/// One profile layer as authored in TOML/YAML — and also the merged result.
///
/// ponytail: one struct for layer+merged; split if a merged-only field appears.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Names of layers this one depends on; pulled in transitively, deps first.
    /// Accepts `extends` as an alias.
    #[serde(default, alias = "extends")]
    pub requires: Vec<String>,
    /// When set, the layer is considered only if every constraint matches the run
    /// context (empty lists = no constraint on that axis).
    #[serde(default)]
    pub filter: Option<ProfileFilter>,
    /// Conditional policy blocks, each gated by its own filter.
    #[serde(default)]
    pub policies: Vec<Policy>,
    /// Unconditional path grants for this layer.
    #[serde(default)]
    pub paths: Vec<PathGrant>,
    /// Default environment variables contributed by this layer (first-writer-wins on merge).
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// HOME substitution settings for this layer.
    #[serde(default)]
    pub home_replace: Option<HomeReplace>,
    /// Command argument rewrites applied when this layer's filter matches.
    #[serde(default)]
    pub rewrite: Option<Rewrite>,
    /// macOS-only Seatbelt extras (capabilities + raw SBPL) for this layer.
    #[serde(default)]
    pub macos: Option<MacosExtra>,
    /// Windows-only AppContainer extras for this layer.
    #[serde(default)]
    pub windows: Option<WindowsExtra>,
}

// Built-in layers — generated by build.rs from profiles/**/*.toml.
include!(concat!(env!("OUT_DIR"), "/profiles_embedded.rs"));

/// Where a profile layer was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerSource {
    /// Embedded at compile time from `profiles/**/*.toml` via `build.rs`.
    Builtin,
    /// Loaded from the user's config directory (`~/.config/isol8/profiles/`).
    UserConfig,
    /// Loaded from an explicit `--profile-path` argument (path stored here).
    ProfilePath(String),
}

#[derive(Debug, Clone)]
struct LayerEntry {
    profile: Profile,
    source: LayerSource,
}

/// Registry of all known layers (builtin + user + profile-path overlays).
pub struct LayerRegistry {
    entries: HashMap<String, LayerEntry>,
}

impl LayerRegistry {
    /// Build a registry from builtin layers, user config, and any explicit `--profile-path` entries.
    pub fn load(profile_paths: &[String]) -> Result<Self> {
        let mut entries = HashMap::new();
        for (name, body) in BUILTIN_PROFILES {
            let layer = parse_layer(name, body, "built-in")?;
            entries.insert(
                (*name).to_string(),
                LayerEntry {
                    profile: layer,
                    source: LayerSource::Builtin,
                },
            );
        }
        load_user_layers(&mut entries)?;
        for path in profile_paths {
            load_profile_path(path, &mut entries)?;
        }
        Ok(Self { entries })
    }

    /// Return all registered profiles as a cloned name-to-`Profile` map.
    pub fn profiles(&self) -> HashMap<String, Profile> {
        self.entries
            .iter()
            .map(|(k, v)| (k.clone(), v.profile.clone()))
            .collect()
    }

    /// Look up a single layer by name, returning a reference to its `Profile`.
    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.entries.get(name).map(|e| &e.profile)
    }

    /// Return the load source for a layer by name.
    pub fn source(&self, name: &str) -> Option<&LayerSource> {
        self.entries.get(name).map(|e| &e.source)
    }

    /// Return all layer names and their sources, sorted alphabetically.
    pub fn list(&self) -> Vec<(String, LayerSource)> {
        let mut names: Vec<_> = self
            .entries
            .iter()
            .map(|(n, e)| (n.clone(), e.source.clone()))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        names
    }
}

/// Parse a TOML layer body, attaching the layer name for clear error messages.
fn parse_layer(name: &str, body: &str, source: &str) -> Result<Profile> {
    toml::from_str::<Profile>(body)
        .ctx(|| format!("failed to parse profile layer '{name}' ({source})"))
}

fn user_config_profiles_dir() -> Option<std::path::PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(base) if !base.is_empty() => {
            Some(std::path::PathBuf::from(base).join("isol8/profiles"))
        }
        _ => std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|h| std::path::PathBuf::from(h).join(".config/isol8/profiles")),
    }
}

/// Discover user-authored layers under the config dir. Silently skipped if absent.
fn load_user_layers(entries: &mut HashMap<String, LayerEntry>) -> Result<()> {
    let Some(dir) = user_config_profiles_dir() else {
        return Ok(());
    };
    if !dir.is_dir() {
        return Ok(());
    }
    load_toml_tree(&dir, &dir, LayerSource::UserConfig, entries)
}

/// Load a profile-path entry (single file or directory). Errors if missing.
fn load_profile_path(path: &str, entries: &mut HashMap<String, LayerEntry>) -> Result<()> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Err(Error::Profile(format!(
            "profile-path does not exist: {path}"
        )));
    }
    let source = LayerSource::ProfilePath(path.to_string());
    if p.is_file() {
        let name = p
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::Profile(format!("invalid profile file '{}'", p.display())))?;
        load_toml_file(p, name, source, entries)?;
    } else {
        load_toml_tree(p, p, source, entries)?;
    }
    Ok(())
}

fn load_toml_tree(
    base: &std::path::Path,
    dir: &std::path::Path,
    source: LayerSource,
    entries: &mut HashMap<String, LayerEntry>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            load_toml_tree(base, &path, source.clone(), entries)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            let rel = path.strip_prefix(base).unwrap_or(&path);
            let name = rel.with_extension("").to_string_lossy().replace('\\', "/");
            load_toml_file(&path, &name, source.clone(), entries)?;
        }
    }
    Ok(())
}

fn load_toml_file(
    path: &std::path::Path,
    name: &str,
    source: LayerSource,
    entries: &mut HashMap<String, LayerEntry>,
) -> Result<()> {
    let body =
        std::fs::read_to_string(path).ctx(|| format!("reading profile '{}'", path.display()))?;
    let layer = parse_layer(name, &body, &path.display().to_string())?;
    entries.insert(
        name.to_string(),
        LayerEntry {
            profile: layer,
            source,
        },
    );
    Ok(())
}

/// Select layer names: explicit profiles + auto-matched executable filters.
pub fn select_layer_names(
    spec: &Spec,
    registry: &LayerRegistry,
    ctx: &RunContext,
) -> Result<Vec<String>> {
    let mut selected: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push = |name: &str| -> Result<()> {
        if !registry.entries.contains_key(name) {
            return Err(Error::Profile(format!(
                "unknown profile '{name}' (not a built-in, user, or profile-path layer)"
            )));
        }
        if seen.insert(name.to_string()) {
            selected.push(name.to_string());
        }
        Ok(())
    };

    for name in &spec.profiles {
        push(name)?;
    }

    if spec.auto_profiles {
        let mut auto_names: Vec<String> = registry
            .entries
            .iter()
            .filter_map(|(name, entry)| {
                if filter::is_auto_selectable(&entry.profile.filter)
                    && entry
                        .profile
                        .filter
                        .as_ref()
                        .is_some_and(|f| filter::filter_matches(f, ctx))
                {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();
        auto_names.sort();
        for name in auto_names {
            push(&name)?;
        }
    }

    Ok(selected)
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
) -> Result<Vec<(String, Profile)>> {
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
                return Err(Error::Profile(format!(
                    "profile dependency cycle detected: {path}"
                )));
            }
            None => {}
        }
        let layer = all.get(name).ok_or_else(|| {
            Error::Profile(format!(
                "unknown profile layer '{name}' referenced via requires"
            ))
        })?;
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
            return Err(Error::Profile(format!(
                "unknown profile '{name}' (not a built-in or user layer)"
            )));
        }
        visit(name, all, &mut state, &mut order, &mut stack)?;
    }

    Ok(order
        .into_iter()
        .map(|n| {
            let p = all[&n].clone();
            (n, p)
        })
        .collect())
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

    let mut windows_caps: Vec<WindowsCapability> = Vec::new();

    let mut ensure_args: Vec<String> = Vec::new();

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

        // rewrite: union ensure_args across layers (first-seen order).
        if let Some(rw) = &layer.rewrite {
            for a in &rw.ensure_args {
                if !ensure_args.contains(a) {
                    ensure_args.push(a.clone());
                }
            }
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

        // windows: union caps.
        if let Some(w) = &layer.windows {
            for c in &w.capabilities {
                if !windows_caps.contains(c) {
                    windows_caps.push(*c);
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

    let rewrite = if ensure_args.is_empty() {
        None
    } else {
        Some(Rewrite { ensure_args })
    };

    let windows = if windows_caps.is_empty() {
        None
    } else {
        Some(WindowsExtra {
            capabilities: windows_caps,
        })
    };

    Profile {
        requires: Vec::new(),
        filter: None,
        policies: Vec::new(),
        paths,
        env,
        home_replace,
        rewrite,
        macos,
        windows,
    }
}

/// Apply a merged `rewrite` to the confined command: insert each missing
/// `ensure_args` entry right after `argv[0]`. Already-present args are left alone.
///
/// ponytail: exact whole-arg match for "present"; doesn't understand `--flag=val`
/// aliases — add normalization only if a profile actually needs it.
pub fn apply_rewrite(cmd: &[String], rewrite: &Option<Rewrite>) -> Vec<String> {
    let mut cmd = cmd.to_vec();
    let Some(rw) = rewrite else { return cmd };
    if cmd.is_empty() {
        return cmd;
    }
    let mut insert_at = 1;
    for arg in &rw.ensure_args {
        if !cmd.contains(arg) {
            cmd.insert(insert_at, arg.clone());
            insert_at += 1;
        }
    }
    cmd
}

/// Build the top invocation-override layer from the auto-granted cwd plus
/// `--add-dirs-rw` / `--add-dirs-ro`.
///
/// The cwd grant is pushed first so an explicit `--add-dirs-*` on the same path
/// still wins (within a layer the later grant for a `(path, match)` key overrides).
fn overrides_layer(spec: &Spec) -> Profile {
    let mut paths = Vec::new();
    // cwd auto-grant: read-write by default, read-only with `--cwd-ro`. Skipped if
    // the cwd can't be read (e.g. it was deleted) — no grant, no panic.
    if let Ok(cwd) = std::env::current_dir() {
        paths.push(PathGrant {
            path: cwd.to_string_lossy().into_owned(),
            access: if spec.cwd_ro { Access::Ro } else { Access::Rw },
            r#match: MatchKind::Subpath,
        });
    }
    for dir in &spec.add_dirs_rw {
        paths.push(PathGrant {
            path: dir.clone(),
            access: Access::Rw,
            r#match: MatchKind::Subpath,
        });
    }
    for dir in &spec.add_dirs_ro {
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

/// Resolve selected layers (deps-first), applying filters. No merge or `~` expansion.
pub fn resolved_layers(spec: &Spec) -> Result<Vec<Profile>> {
    let registry = LayerRegistry::load(&spec.profile_paths)?;
    let ctx = RunContext::from_cmd(&spec.cmd);
    let names = select_layer_names(spec, &registry, &ctx)?;
    let layers = resolve_requires(&names, &registry.profiles())?;
    Ok(layers
        .into_iter()
        .map(|(_, l)| filter::apply_layer_filter(l, &ctx))
        .collect())
}

/// Merge resolved layers + invocation overrides into one effective profile.
pub fn load_merged(
    spec: &Spec,
    layers: &[Profile],
    home: &EffectiveHome,
    _ctx: &RunContext,
) -> Result<Profile> {
    let mut expanded: Vec<Profile> = layers
        .iter()
        .cloned()
        .map(|mut layer| {
            for grant in &mut layer.paths {
                grant.path = home::expand_grant(&grant.path, &home.path);
            }
            layer
        })
        .collect();

    let mut over = overrides_layer(spec);
    for grant in &mut over.paths {
        grant.path = home::expand_grant(&grant.path, &home.path);
    }
    expanded.push(over);
    Ok(merge(&expanded))
}

/// Load profile layers, resolve inheritance, expand `~`, merge deny-first.
pub fn load(spec: &Spec, home: &EffectiveHome) -> Result<Profile> {
    let layers = resolved_layers(spec)?;
    let ctx = RunContext::from_cmd(&spec.cmd);
    load_merged(spec, &layers, home, &ctx)
}

/// Serialize a layer back to TOML for `profiles show`.
pub fn format_layer(profile: &Profile) -> Result<String> {
    toml::to_string_pretty(profile).ctx(|| "serializing profile layer")
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

    fn run_args(cwd_ro: bool) -> Spec {
        crate::cli::run_from(
            crate::cli::ProfileOpts {
                cwd_ro,
                ..Default::default()
            },
            vec!["echo".into(), "hi".into()],
        )
    }

    #[test]
    fn overrides_layer_grants_cwd_rw_by_default() {
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let layer = overrides_layer(&run_args(false));
        let g = find(&layer, &cwd).expect("cwd granted");
        assert_eq!(g.access, Access::Rw);
    }

    #[test]
    fn overrides_layer_cwd_ro_downgrades() {
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let layer = overrides_layer(&run_args(true));
        let g = find(&layer, &cwd).expect("cwd granted");
        assert_eq!(g.access, Access::Ro);
    }

    #[test]
    fn merge_rewrite_unions_ensure_args() {
        let low = Profile {
            rewrite: Some(Rewrite {
                ensure_args: vec!["--a".into(), "--b".into()],
            }),
            ..Default::default()
        };
        let high = Profile {
            rewrite: Some(Rewrite {
                ensure_args: vec!["--b".into(), "--c".into()],
            }),
            ..Default::default()
        };
        let merged = merge(&[low, high]);
        let rw = merged.rewrite.unwrap();
        assert_eq!(rw.ensure_args, vec!["--a", "--b", "--c"]); // deduped, first-seen order
    }

    #[test]
    fn merge_windows_caps_union() {
        let low = Profile {
            windows: Some(WindowsExtra {
                capabilities: vec![WindowsCapability::InternetClient],
            }),
            ..Default::default()
        };
        let high = Profile {
            windows: Some(WindowsExtra {
                capabilities: vec![
                    WindowsCapability::InternetClient, // duplicate
                    WindowsCapability::PrivateNetworkClientServer,
                ],
            }),
            ..Default::default()
        };
        let merged = merge(&[low, high]);
        let w = merged.windows.unwrap();
        assert_eq!(w.capabilities.len(), 2);
        assert!(w.capabilities.contains(&WindowsCapability::InternetClient));
        assert!(w
            .capabilities
            .contains(&WindowsCapability::PrivateNetworkClientServer));
    }

    #[test]
    fn merge_windows_none_when_no_layers_set_it() {
        let merged = merge(&[Profile::default(), Profile::default()]);
        assert!(merged.windows.is_none());
    }

    #[test]
    fn apply_rewrite_inserts_missing_after_argv0() {
        let rw = Some(Rewrite {
            ensure_args: vec!["--skip".into(), "--yes".into()],
        });
        let cmd = vec!["claude".into(), "-p".into(), "hi".into()];
        let out = apply_rewrite(&cmd, &rw);
        assert_eq!(out, vec!["claude", "--skip", "--yes", "-p", "hi"]);
    }

    #[test]
    fn apply_rewrite_skips_already_present() {
        let rw = Some(Rewrite {
            ensure_args: vec!["--skip".into()],
        });
        let cmd = vec!["claude".into(), "--skip".into()];
        let out = apply_rewrite(&cmd, &rw);
        assert_eq!(out, vec!["claude", "--skip"]); // unchanged
    }

    #[test]
    fn apply_rewrite_none_and_empty_cmd_are_noops() {
        let cmd = vec!["claude".into()];
        assert_eq!(apply_rewrite(&cmd, &None), cmd);
        let rw = Some(Rewrite {
            ensure_args: vec!["--x".into()],
        });
        assert!(apply_rewrite(&[], &rw).is_empty());
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
        assert_eq!(order[0].0, "base"); // base first
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

    #[test]
    fn parse_filter_os_and_executables() {
        let p: Profile = toml::from_str(
            r#"
filter = { os = ["linux"], executables = ["claude"] }
paths = []
"#,
        )
        .unwrap();
        let f = p.filter.unwrap();
        assert_eq!(f.os, ["linux"]);
        assert_eq!(f.executables, ["claude"]);
        assert!(f.arch.is_empty());
    }

    #[test]
    fn resolve_linux_system_alias_pulls_system_runtime() {
        let registry = LayerRegistry::load(&[]).unwrap();
        let all = registry.profiles();
        let order = resolve_requires(&["linux-system".into()], &all).unwrap();
        assert_eq!(order.len(), 3); // base → linux/system-runtime → linux-system
        assert!(order[0].1.requires.is_empty()); // base
        assert_eq!(order[1].1.requires, vec!["base".to_string()]); // linux/system-runtime
        assert_eq!(
            order[2].1.requires,
            vec!["linux/system-runtime".to_string()]
        );
    }

    #[test]
    fn all_builtin_profiles_parse() {
        let registry = LayerRegistry::load(&[]).unwrap();
        assert!(registry.list().len() >= 60);
    }
}
