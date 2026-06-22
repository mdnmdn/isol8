//! Filter matching and layer/policy application for conditional profiles.

use std::path::Path;

use crate::profile::{MacosExtra, Profile, ProfileFilter, WindowsExtra};

/// Runtime context used to evaluate profile filters.
#[derive(Debug, Clone)]
pub struct RunContext {
    /// Argv of the confined command (index 0 is the executable).
    pub cmd: Vec<String>,
    /// Normalized OS identifier (`macos`, `linux`, `windows`, …).
    pub os: String,
    /// CPU architecture string as reported by `std::env::consts::ARCH`.
    pub arch: String,
}

impl RunContext {
    /// Build a `RunContext` from a command argv, deriving OS and arch from the host.
    pub fn from_cmd(cmd: &[String]) -> Self {
        Self {
            cmd: cmd.to_vec(),
            os: map_os(std::env::consts::OS),
            arch: std::env::consts::ARCH.to_string(),
        }
    }
}

fn map_os(os: &str) -> String {
    match os {
        "macos" => "macos".into(),
        "linux" => "linux".into(),
        "windows" => "windows".into(),
        other => other.into(),
    }
}

/// Basename of the command executable (strips path and `.exe`).
pub fn executable_basename(cmd: &[String]) -> Option<String> {
    let first = cmd.first()?;
    let path = Path::new(first);
    let stem = path.file_stem()?.to_str()?;
    Some(stem.to_string())
}

/// True when every non-empty constraint in `filter` matches `ctx`.
pub fn filter_matches(filter: &ProfileFilter, ctx: &RunContext) -> bool {
    if !filter.os.is_empty() && !filter.os.iter().any(|o| o == &ctx.os) {
        return false;
    }
    if !filter.arch.is_empty() && !filter.arch.iter().any(|a| a == &ctx.arch) {
        return false;
    }
    if !filter.executables.is_empty() {
        let Some(exe) = executable_basename(&ctx.cmd) else {
            return false;
        };
        if !filter
            .executables
            .iter()
            .any(|e| e == &exe || e == &ctx.cmd[0])
        {
            return false;
        }
    }
    true
}

/// True when `filter` has at least one constraint axis set.
pub fn filter_is_active(filter: &ProfileFilter) -> bool {
    !filter.os.is_empty() || !filter.arch.is_empty() || !filter.executables.is_empty()
}

/// Layer is auto-selectable when it has an executable constraint.
pub fn is_auto_selectable(filter: &Option<ProfileFilter>) -> bool {
    filter.as_ref().is_some_and(|f| !f.executables.is_empty())
}

/// Fold matching `[[policies]]` into unconditional layer fields; drop non-matching.
pub fn apply_policies(mut layer: Profile, ctx: &RunContext) -> Profile {
    for policy in &layer.policies {
        if !filter_matches(&policy.filter, ctx) {
            continue;
        }
        layer.paths.extend(policy.paths.clone());
        if let Some(m) = &policy.macos {
            match &mut layer.macos {
                Some(existing) => merge_macos(existing, m),
                None => layer.macos = Some(m.clone()),
            }
        }
        if let Some(w) = &policy.windows {
            match &mut layer.windows {
                Some(existing) => merge_windows(existing, w),
                None => layer.windows = Some(w.clone()),
            }
        }
    }
    layer.policies.clear();
    layer
}

fn merge_windows(dst: &mut WindowsExtra, src: &WindowsExtra) {
    for c in &src.capabilities {
        if !dst.capabilities.contains(c) {
            dst.capabilities.push(*c);
        }
    }
}

fn merge_macos(dst: &mut MacosExtra, src: &MacosExtra) {
    for c in &src.capabilities {
        if !dst.capabilities.contains(c) {
            dst.capabilities.push(*c);
        }
    }
    if !src.raw.is_empty() {
        dst.raw.push_str(&src.raw);
        if !src.raw.ends_with('\n') {
            dst.raw.push('\n');
        }
    }
}

/// If the layer-level filter fails, return an empty content shell (requires kept).
pub fn apply_layer_filter(mut layer: Profile, ctx: &RunContext) -> Profile {
    if let Some(ref f) = layer.filter {
        if !filter_matches(f, ctx) {
            layer.paths.clear();
            layer.env.clear();
            layer.home_replace = None;
            layer.rewrite = None;
            layer.macos = None;
            layer.windows = None;
            layer.policies.clear();
            return layer;
        }
    }
    apply_policies(layer, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Access, PathGrant, Policy, WindowsCapability, WindowsExtra};

    fn ctx(cmd: &str) -> RunContext {
        RunContext {
            cmd: vec![cmd.into()],
            os: "macos".into(),
            arch: "aarch64".into(),
        }
    }

    #[test]
    fn executable_basename_strips_path() {
        assert_eq!(
            executable_basename(&["/usr/bin/claude".into()]),
            Some("claude".into())
        );
    }

    #[test]
    fn filter_os_and_executable_and() {
        let f = ProfileFilter {
            os: vec!["macos".into()],
            executables: vec!["claude".into()],
            ..Default::default()
        };
        assert!(filter_matches(&f, &ctx("claude")));
        assert!(!filter_matches(
            &f,
            &RunContext {
                cmd: vec!["claude".into()],
                os: "linux".into(),
                arch: "x86_64".into(),
            }
        ));
        assert!(!filter_matches(&f, &ctx("cargo")));
    }

    #[test]
    fn apply_layer_filter_clears_on_mismatch() {
        let layer = Profile {
            filter: Some(ProfileFilter {
                os: vec!["linux".into()],
                ..Default::default()
            }),
            paths: vec![PathGrant {
                path: "/x".into(),
                access: Access::Ro,
                r#match: Default::default(),
            }],
            ..Default::default()
        };
        let out = apply_layer_filter(layer, &ctx("sh"));
        assert!(out.paths.is_empty());
    }

    #[test]
    fn apply_policies_folds_matching() {
        let layer = Profile {
            policies: vec![Policy {
                filter: ProfileFilter {
                    executables: vec!["claude".into()],
                    ..Default::default()
                },
                paths: vec![PathGrant {
                    path: "~/.claude".into(),
                    access: Access::Rw,
                    r#match: Default::default(),
                }],
                macos: None,
                windows: None,
            }],
            ..Default::default()
        };
        let out = apply_layer_filter(layer, &ctx("claude"));
        assert_eq!(out.paths.len(), 1);
        assert!(out.policies.is_empty());
    }

    #[test]
    fn merge_windows_union() {
        let mut dst = WindowsExtra {
            capabilities: vec![WindowsCapability::InternetClient],
        };
        let src = WindowsExtra {
            capabilities: vec![
                WindowsCapability::InternetClient, // duplicate
                WindowsCapability::InternetClientServer,
            ],
        };
        merge_windows(&mut dst, &src);
        assert_eq!(dst.capabilities.len(), 2);
        assert!(dst
            .capabilities
            .contains(&WindowsCapability::InternetClient));
        assert!(dst
            .capabilities
            .contains(&WindowsCapability::InternetClientServer));
    }

    #[test]
    fn apply_policies_folds_windows_caps() {
        let layer = Profile {
            policies: vec![Policy {
                filter: ProfileFilter {
                    executables: vec!["claude".into()],
                    ..Default::default()
                },
                windows: Some(WindowsExtra {
                    capabilities: vec![WindowsCapability::InternetClient],
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let out = apply_layer_filter(layer, &ctx("claude"));
        let w = out.windows.unwrap();
        assert_eq!(w.capabilities, vec![WindowsCapability::InternetClient]);
    }

    #[test]
    fn apply_layer_filter_clears_windows_on_os_mismatch() {
        let layer = Profile {
            filter: Some(ProfileFilter {
                os: vec!["linux".into()],
                ..Default::default()
            }),
            windows: Some(WindowsExtra {
                capabilities: vec![WindowsCapability::InternetClient],
            }),
            ..Default::default()
        };
        let out = apply_layer_filter(layer, &ctx("sh"));
        assert!(out.windows.is_none());
    }
}
