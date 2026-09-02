//! Harness integration: one confined session per agent, driven entirely from
//! host state (no `isol8.toml`, no `ISOL8_*`, no interactive prompts).
//!
//! This is the shape an agent manager wants: the host owns the session, the
//! workspace and the lifetime; isol8 owns the policy. See
//! [`_docs/integration.md`](../_docs/integration.md).
//!
//! ```sh
//! cargo run --example embed_harness
//! ISOL8_EXAMPLE_SPAWN=1 cargo run --example embed_harness   # actually run the child
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use isol8::profile::{Access, MatchKind, PathGrant, Profile};
use isol8::registry::{ProfileSource, RegistryIndex, TrustLevel};
use isol8::{Config, Context, HomeOpSpec, Platform, Recipe, Spec};

/// What the host decides for one agent session.
struct SessionSpec {
    /// Single path segment: names the generated layer and the managed home.
    id: String,
    /// The only directory the agent may write to.
    workspace: PathBuf,
    /// Extra read-only grants (shared caches, a read-only monorepo, …).
    extra_ro: Vec<PathBuf>,
}

/// Host-owned state root. Everything isol8 reads or writes lives under here, so
/// the harness stays independent of the user's `~/.config/isol8`.
struct Harness {
    state_dir: PathBuf,
}

impl Harness {
    fn new(state_dir: PathBuf) -> isol8::Result<Self> {
        std::fs::create_dir_all(state_dir.join("profiles"))?;
        Ok(Self { state_dir })
    }

    /// A [`Context`] built from host state, never from the process environment.
    ///
    /// `cwd` is the session workspace — isol8 auto-grants it, and relative
    /// grants resolve against it.
    fn context(&self, session: &SessionSpec) -> Context {
        Context {
            real_home: isol8::home::real_home(),
            cwd: session.workspace.clone(),
            platform: Platform::current(),
            config_dir: self.state_dir.clone(),
            managed_root: self.state_dir.join("homes"),
        }
    }

    /// The host's baseline, replacing config-file discovery entirely.
    fn config(&self) -> Config {
        let mut cfg = Config::builtin_defaults(); // base + this OS's system runtime
        cfg.auto_profiles = true; // pick up agents/* by executable name
        cfg.profile_paths = vec![self.state_dir.join("profiles").display().to_string()];
        cfg
    }

    /// Build a per-session layer in memory and write it where `profile_paths`
    /// will find it. The layer name is the path under the profile root minus
    /// `.toml` — so this one is `harness/<id>`.
    fn write_session_layer(&self, session: &SessionSpec) -> isol8::Result<String> {
        let mut paths = vec![PathGrant {
            path: session.workspace.display().to_string(),
            access: Access::Rw,
            r#match: MatchKind::Subpath,
        }];
        for ro in &session.extra_ro {
            paths.push(PathGrant {
                path: ro.display().to_string(),
                access: Access::Ro,
                r#match: MatchKind::Subpath,
            });
        }

        let layer = Profile {
            paths,
            env: HashMap::from([("ISOL8_SESSION".to_string(), session.id.clone())]),
            ..Default::default()
        };

        let dir = self.state_dir.join("profiles").join("harness");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!("{}.toml", session.id));
        std::fs::write(&file, isol8::profile::format_layer(&layer)?)?;
        Ok(format!("harness/{}", session.id))
    }

    /// Assemble the [`Spec`] for one session.
    fn spec(&self, session: &SessionSpec, cmd: &[&str]) -> isol8::Result<Spec> {
        let ctx = self.context(session);
        let mut cfg = self.config();
        let layer = self.write_session_layer(session)?;

        // Each `Spec` field is filled from the config only when it is *empty* —
        // so append the session layer to the defaults rather than pre-setting
        // `base.profiles`, which would drop `base` and the OS system runtime.
        cfg.default_profiles.push(layer);

        // Pre-set fields win over both the cage overlay and the config.
        let mut base = Spec::default();
        // A private home per session, under {state_dir}/homes/<id>.
        base.home = Some(format!("@managed/{}", session.id));
        base.home_ops = vec![
            HomeOpSpec::mkdir("~/.cache"),
            HomeOpSpec::mkdir("~/.local/share"),
        ];

        let cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        isol8::resolve::spec_from_config(&cfg, base, cmd, &ctx)
    }
}

// ---------------------------------------------------------------------------
// A custom catalog source. `ProfileSource` is the one trait an embedder may
// implement — it feeds discovery and install UX, not the resolve pipeline.
// ---------------------------------------------------------------------------

struct StaticSource {
    index: RegistryIndex,
    recipes: HashMap<String, String>,
}

impl ProfileSource for StaticSource {
    fn name(&self) -> &str {
        "harness-builtin"
    }
    fn index(&self) -> &RegistryIndex {
        &self.index
    }
    fn trust(&self) -> TrustLevel {
        // Untrusted sources cannot run detect/verify commands.
        TrustLevel::Local
    }
    fn root(&self) -> Option<&Path> {
        None
    }
    fn get_recipe(&self, id: &str) -> isol8::Result<Option<Recipe>> {
        match self.recipes.get(id) {
            Some(body) => Ok(Some(isol8::recipe::parse_recipe(body, self.name())?)),
            None => Ok(None),
        }
    }
    fn get_profile(&self, _id: &str) -> isol8::Result<Option<Profile>> {
        // `Profile` is `Deserialize`; parse with `toml` if your source ships layers.
        Ok(None)
    }
}

fn main() -> isol8::Result<()> {
    // Only needed if the host also honours `[registries.*]` from a config file.
    isol8::ensure_registry_provider();

    let root = std::env::temp_dir().join("isol8-harness-example");
    let workspace = root.join("workspaces").join("s1");
    std::fs::create_dir_all(&workspace)?;

    let harness = Harness::new(root.join("state"))?;
    let session = SessionSpec {
        id: "s1".to_string(),
        workspace: workspace.clone(),
        extra_ro: vec![],
    };

    let spec = harness.spec(&session, &["/bin/echo", "confined"])?;
    let ctx = harness.context(&session);

    // Audit before running: hermetic, side-effect free, nothing is written.
    let dry = isol8::sandbox::dry_run_in(&spec, &ctx)?;
    println!("== session {} ==", session.id);
    println!("  home   : {}", dry.home_path.display());
    println!("  cmd    : {}", dry.cmd.join(" "));
    println!("  layers :");
    for (name, origin) in &dry.layer_names {
        println!("    {name} ({origin:?})");
    }
    println!("  grants :");
    for g in &dry.profile.paths {
        println!("    {:?} {}", g.access, g.path);
    }
    println!("  plan   :");
    print!("{}", dry.home_plan.render());

    // The custom source is a catalog: enumerate and inspect, then install the
    // artifacts you accept into a directory the engine reads.
    let custom = StaticSource {
        index: RegistryIndex {
            schema: 1,
            registry: "harness-builtin".into(),
            count: 0,
            entries: vec![],
        },
        recipes: HashMap::new(),
    };
    println!(
        "\ncatalog '{}': {} entr(ies), trust {:?}, commands allowed: {}",
        custom.name(),
        custom.index().entries.len(),
        custom.trust(),
        custom.trust().commands_allowed()
    );

    // Spawning is opt-in: it applies the home plan and launches a real child.
    if std::env::var("ISOL8_EXAMPLE_SPAWN").is_ok() {
        let mut child = isol8::Sandbox::from_spec(spec.clone()).spawn(spec.cmd.clone())?;
        println!("\nspawned pid {}", child.id());
        println!("exit {}", child.wait()?);
    } else {
        println!("\n(set ISOL8_EXAMPLE_SPAWN=1 to spawn the confined child)");
    }

    Ok(())
}
