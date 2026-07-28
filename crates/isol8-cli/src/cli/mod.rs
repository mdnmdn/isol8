use clap::{CommandFactory, Parser, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;

/// Prefix for meta subcommands (not passed to the confined process).
pub const META_PREFIX: &str = "@";

/// Return the binary version string (`ISOL8_VERSION` env override, else `CARGO_PKG_VERSION`).
pub fn version_string() -> &'static str {
    option_env!("ISOL8_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Clap-parsed confinement options shared across the `run`, `diag`, and introspection paths.
#[derive(Parser, Clone, Default)]
#[command(
    name = "isol8",
    version = version_string(),
    about = "Lightweight cross-platform isolation sandbox for agents and CLI tools",
    override_usage = "isol8 [OPTIONS] <COMMAND>...\n       isol8 @<meta-command> [OPTIONS] [ARGS]...\n       isol8 --help"
)]
pub struct ProfileOpts {
    /// Named profile layer to enable (repeatable, deny-first merge order).
    #[arg(long = "profile")]
    pub profiles: Vec<String>,

    /// Extra profile directory or single TOML file (repeatable; overrides same-named layers).
    #[arg(long = "profile-path")]
    pub profile_paths: Vec<String>,

    /// Auto-select layers whose executable filter matches the command.
    #[arg(long = "auto-profiles", default_value_t = false, action = clap::ArgAction::SetTrue)]
    pub auto_profiles: bool,

    /// Disable auto-selection (overrides config `auto_profiles = true`).
    #[arg(
        long = "no-auto-profiles",
        default_value_t = false,
        action = clap::ArgAction::SetTrue,
        conflicts_with = "auto_profiles"
    )]
    pub no_auto_profiles: bool,

    /// Grant read-write access to a path (repeatable).
    #[arg(long = "add-dirs-rw")]
    pub add_dirs_rw: Vec<String>,

    /// Grant read-only access to a path (repeatable).
    #[arg(long = "add-dirs-ro")]
    pub add_dirs_ro: Vec<String>,

    /// Make the auto-granted current working directory read-only (default: read-write).
    #[arg(long = "cwd-ro", default_value_t = false, action = clap::ArgAction::SetTrue)]
    pub cwd_ro: bool,

    /// Named cage (isolation unit) to load; overrides config `cage` / discovery default.
    #[arg(short = 'c', long = "cage", value_name = "NAME")]
    pub cage: Option<String>,

    /// Toolchain recipe choices from a cage (filled by apply_cage_to_opts; not a clap flag).
    #[arg(skip)]
    pub toolchains: Vec<isol8_core::recipe::ToolchainChoice>,

    /// Replacement $HOME (defaults to an auto scratch home when a profile enables it).
    #[arg(long)]
    pub home: Option<String>,

    /// Use a temporary scratch home (from cage `home = "ephemeral"`, or set explicitly).
    /// Hidden from help — prefer `--home` or a cage; kept for Spec round-trip.
    #[arg(long = "ephemeral-home", default_value_t = false, action = clap::ArgAction::SetTrue, hide = true)]
    pub ephemeral_home: bool,

    /// Skip seeding real-home files into the (replacement) home (overrides profile seed lists).
    #[arg(long = "no-seed", default_value_t = false, action = clap::ArgAction::SetTrue)]
    pub no_seed: bool,

    /// Pass a named variable through from the host env (repeatable; overrides profile env).
    #[arg(long = "env-pass", value_name = "NAME")]
    pub env_pass: Vec<String>,

    /// Set an env var explicitly (repeatable; `K=V`; highest precedence).
    #[arg(long = "set-env", value_name = "K=V")]
    pub set_env: Vec<String>,

    /// Print the effective merged policy (layer stack, grants, env, SBPL) and exit.
    #[arg(long = "show-policies")]
    pub show_policies: bool,

    /// List all profile layers, or — when a command is given — show which layers apply.
    #[arg(long = "show-profiles")]
    pub show_profiles: bool,

    /// Alias for --show-policies.
    #[arg(long)]
    pub dry_run: bool,

    /// Run the command, collect denials (when available), suggest recipes.
    ///
    /// Denials come from `ISOL8_ANALYZE_FEED` (NDJSON), a post-run
    /// `$TMP/isol8-analyze-<pid>.ndjson` file, macOS unified log (Phase 6),
    /// or (later) other platform observers.
    #[arg(long = "analyze", default_value_t = false, action = clap::ArgAction::SetTrue)]
    pub analyze: bool,

    /// With `--analyze` on macOS: inject Seatbelt `(trace …)` (permissive) and
    /// write a draft allow profile. Explicit opt-in only — never a default.
    #[arg(long = "author", default_value_t = false, action = clap::ArgAction::SetTrue)]
    pub author: bool,

    /// Emit machine-readable JSON instead of the text report.
    ///
    /// Applies to `--show-policies`, `--analyze`, and the `@cage` / `@registry` /
    /// `@profiles-*` meta-commands. The shape is the engine type itself, so a
    /// non-Rust host parses exactly what an embedder would see.
    #[arg(long = "json", default_value_t = false, action = clap::ArgAction::SetTrue)]
    pub json: bool,

    /// Verbose output for --show-profiles (list mode).
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

/// Top-level parsed invocation: confinement options plus the command to run.
#[derive(Parser)]
pub struct RunInvocation {
    /// Confinement options (profiles, paths, env, home, …).
    #[command(flatten)]
    pub opts: ProfileOpts,

    /// Command and arguments to confine.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub cmd: Vec<String>,
}

impl ProfileOpts {
    /// Explicit CLI override for auto-profile selection, if any.
    pub fn auto_profiles_cli_override(&self) -> Option<bool> {
        if self.auto_profiles {
            Some(true)
        } else if self.no_auto_profiles {
            Some(false)
        } else {
            None
        }
    }
}

impl RunInvocation {
    /// Named profile layers requested via `--profile`.
    pub fn profiles(&self) -> &[String] {
        &self.opts.profiles
    }
    /// Extra profile directories or files requested via `--profile-path`.
    pub fn profile_paths(&self) -> &[String] {
        &self.opts.profile_paths
    }
    /// Whether auto-profile selection is enabled.
    pub fn auto_profiles(&self) -> bool {
        self.opts.auto_profiles
    }
    /// Paths granted read-write access via `--add-dirs-rw`.
    pub fn add_dirs_rw(&self) -> &[String] {
        &self.opts.add_dirs_rw
    }
    /// Paths granted read-only access via `--add-dirs-ro`.
    pub fn add_dirs_ro(&self) -> &[String] {
        &self.opts.add_dirs_ro
    }
    /// Whether the current working directory is confined to read-only.
    pub fn cwd_ro(&self) -> bool {
        self.opts.cwd_ro
    }
    /// Optional replacement `$HOME` path.
    pub fn home(&self) -> &Option<String> {
        &self.opts.home
    }

    /// True when `--show-policies` or `--dry-run` was passed.
    pub fn show_policies(&self) -> bool {
        self.opts.show_policies || self.opts.dry_run
    }

    /// True when `--show-profiles` was passed.
    pub fn show_profiles(&self) -> bool {
        self.opts.show_profiles
    }

    /// True when `-v` / `--verbose` was passed.
    pub fn verbose(&self) -> bool {
        self.opts.verbose
    }
}

impl ProfileOpts {
    /// Convert parsed CLI options + command into the clap-free engine [`Spec`](isol8_core::sandbox::Spec).
    pub fn into_spec(self, cmd: Vec<String>) -> isol8_core::sandbox::Spec {
        let mut spec = isol8_core::sandbox::Spec::new(cmd);
        spec.profiles = self.profiles;
        spec.profile_paths = self.profile_paths;
        spec.auto_profiles = self.auto_profiles;
        spec.add_dirs_rw = self.add_dirs_rw;
        spec.add_dirs_ro = self.add_dirs_ro;
        spec.cwd_ro = self.cwd_ro;
        spec.home = self.home;
        spec.ephemeral_home = self.ephemeral_home;
        spec.no_seed = self.no_seed;
        spec.toolchains = self.toolchains;
        spec.env_pass = self.env_pass;
        spec.set_env = self.set_env;
        spec
    }
}

/// Build the engine [`Spec`](isol8_core::sandbox::Spec) from options + command (CLI / test convenience).
pub fn run_from(opts: ProfileOpts, cmd: Vec<String>) -> isol8_core::sandbox::Spec {
    opts.into_spec(cmd)
}

/// Arguments for the `@init` meta-command (write a default config file).
#[derive(Parser)]
pub struct InitArgs {
    /// Directory or file path for the config (default: OS config dir / isol8.toml).
    #[arg(long)]
    pub path: Option<String>,

    /// Output format for the generated config file (`toml` or `yaml`).
    #[arg(long, default_value = "toml")]
    pub format: ConfigFormat,
}

/// Config file format written by `@init`.
#[derive(Clone, Copy, ValueEnum, Default)]
pub enum ConfigFormat {
    /// TOML format (default).
    #[default]
    Toml,
    /// YAML format.
    Yaml,
}

/// Arguments for the `@diag` meta-command (diagnose sandbox launch failures).
#[derive(Parser)]
pub struct DiagArgs {
    /// Confinement options forwarded to the diagnostics engine.
    #[command(flatten)]
    pub opts: ProfileOpts,

    /// Command to diagnose under the sandbox.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub cmd: Vec<String>,
}

/// Arguments for the `@profiles-list` meta-command.
#[derive(Parser)]
pub struct ProfilesListArgs {
    /// Confinement options (used for `--profile-path` and `--verbose`).
    #[command(flatten)]
    pub opts: ProfileOpts,
}

/// Arguments for the `@profiles-show` meta-command.
#[derive(Parser)]
pub struct ProfilesShowArgs {
    /// Name of the profile layer to display.
    pub name: String,

    /// Confinement options (used for `--profile-path`).
    #[command(flatten)]
    pub opts: ProfileOpts,
}

/// Arguments for `@cage list|show|new|edit|detect|verify`.
#[derive(Parser, Debug)]
pub struct CageArgs {
    /// Subcommand: list | show | new | edit | detect | verify.
    pub action: String,
    /// Cage name (required for show/new/edit; optional for verify).
    pub name: Option<String>,
    /// Home mode for `@cage new`/`edit`: `inherit`, `ephemeral`, `managed`,
    /// `@managed/<id>`, or a path.
    #[arg(long = "home", default_value = "inherit")]
    pub home: String,
    /// Directory to write `@cage new` into (default: user cages dir).
    #[arg(long = "path")]
    pub path: Option<String>,
    /// Non-interactive: accept defaults / provided flags without prompts.
    #[arg(long = "yes", short = 'y')]
    pub yes: bool,
    /// Comma-separated toolchains (`nvm,cargo:share,maven`). Bare ids use recipe defaults.
    #[arg(long = "tools")]
    pub tools: Option<String>,
    /// Extra project dir to grant `rw` (repeatable).
    #[arg(long = "dir")]
    pub dirs: Vec<String>,
    /// Seed from a bundle id or path (`bundles/polyglot-agent`, `./bundle.toml`).
    #[arg(long = "from")]
    pub from: Option<String>,
    /// Overwrite existing cage / ignore managed-section drift.
    #[arg(long = "force")]
    pub force: bool,
    /// Print generated TOML only (do not write).
    #[arg(long = "preview")]
    pub preview: bool,
    /// Run `@cage verify` after a successful write.
    #[arg(long = "verify")]
    pub verify: bool,
    /// Comma-separated profile layers (overrides empty default).
    #[arg(long = "profiles")]
    pub profiles: Option<String>,

    /// Emit machine-readable JSON instead of the text report.
    #[arg(long = "json", default_value_t = false, action = clap::ArgAction::SetTrue)]
    pub json: bool,
}

/// Arguments for `@registry list|update|install|show`.
#[derive(Parser, Debug)]
pub struct RegistryArgs {
    /// Subcommand: list | update | install | show.
    pub action: String,
    /// Registry name (optional for list/update all; required for show id lookup as name or id).
    pub name: Option<String>,
    /// Write lockfile even when only listing (unused).
    #[arg(long = "lockfile")]
    pub lockfile: Option<String>,
    /// Skip writing the lockfile on update/install.
    #[arg(long = "no-lock")]
    pub no_lock: bool,
    /// Treat ceiling / forbidden-path flags as hard errors on install.
    #[arg(long = "strict")]
    pub strict: bool,

    /// Emit machine-readable JSON instead of the text report.
    #[arg(long = "json", default_value_t = false, action = clap::ArgAction::SetTrue)]
    pub json: bool,
}

/// Top-level parse result.
pub enum ParsedCli {
    /// No arguments — print help.
    Help,
    /// Confine and run (or introspect via --show-* flags).
    Run(RunInvocation),
    /// Write a default config file (`@init`).
    Init(InitArgs),
    /// List available profile layers (`@profiles-list`).
    ProfilesList(ProfilesListArgs),
    /// Show one profile layer as TOML (`@profiles-show`).
    ProfilesShow(ProfilesShowArgs),
    /// Diagnose a sandbox launch failure (`@diag`).
    Diag(DiagArgs),
    /// Cage admin (`@cage list|show|new`).
    Cage(CageArgs),
    /// Registry admin (`@registry list|update|install|show`).
    Registry(RegistryArgs),
    /// Print version and exit.
    Version,
}

/// Parse `std::env::args_os` into a [`ParsedCli`], handling meta-commands and help/version exits.
pub fn parse() -> ParsedCli {
    let raw: Vec<OsString> = std::env::args_os().skip(1).collect();
    if raw.is_empty() {
        return ParsedCli::Help;
    }

    let first = raw[0].to_string_lossy();
    if let Some(meta) = first.strip_prefix(META_PREFIX) {
        return parse_meta(meta, &raw[1..]);
    }

    let mut argv: Vec<OsString> = vec![OsString::from("isol8")];
    argv.extend(raw);
    match RunInvocation::try_parse_from(&argv) {
        Ok(run) => {
            if run.cmd.is_empty() && !run.show_policies() && !run.show_profiles() {
                ParsedCli::Help
            } else {
                ParsedCli::Run(run)
            }
        }
        Err(e) if e.kind() == clap::error::ErrorKind::DisplayHelp => ParsedCli::Help,
        Err(e) if e.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            ParsedCli::Help
        }
        Err(e) => {
            e.exit();
        }
    }
}

fn parse_meta(name: &str, rest: &[OsString]) -> ParsedCli {
    let mut argv: Vec<OsString> = vec![OsString::from("isol8")];
    argv.extend_from_slice(rest);

    match name {
        "init" => match InitArgs::try_parse_from(&argv) {
            Ok(a) => ParsedCli::Init(a),
            Err(e) => e.exit(),
        },
        "version" => ParsedCli::Version,
        "diag" => match DiagArgs::try_parse_from(&argv) {
            Ok(a) => ParsedCli::Diag(a),
            Err(e) => e.exit(),
        },
        "profiles-list" => match ProfilesListArgs::try_parse_from(&argv) {
            Ok(a) => ParsedCli::ProfilesList(a),
            Err(e) => e.exit(),
        },
        "profiles-show" => {
            if rest.is_empty() {
                eprintln!("error: @profiles-show requires a layer name");
                eprintln!("usage: isol8 @profiles-show <NAME> [OPTIONS]");
                std::process::exit(2);
            }
            let mut show_argv = vec![OsString::from("isol8")];
            show_argv.push(rest[0].clone());
            show_argv.extend_from_slice(&rest[1..]);
            match ProfilesShowArgs::try_parse_from(&show_argv) {
                Ok(a) => ParsedCli::ProfilesShow(a),
                Err(e) => e.exit(),
            }
        }
        "cage" => {
            if rest.is_empty() {
                eprintln!("error: @cage requires an action");
                eprintln!("usage: isol8 @cage list");
                eprintln!("       isol8 @cage show <NAME>");
                eprintln!("       isol8 @cage new <NAME> [OPTIONS]");
                eprintln!("       isol8 @cage edit <NAME> [OPTIONS]");
                eprintln!("       isol8 @cage detect");
                eprintln!("       isol8 @cage verify [NAME]");
                eprintln!();
                eprintln!("  @cage new options:");
                eprintln!("    --home inherit|ephemeral|managed|PATH");
                eprintln!("    --tools nvm,cargo:share   --dir ~/proj  --from bundles/…");
                eprintln!("    --yes  --force  --preview  --verify  --path DIR");
                std::process::exit(2);
            }
            let mut cage_argv = vec![OsString::from("isol8")];
            cage_argv.extend_from_slice(rest);
            match CageArgs::try_parse_from(&cage_argv) {
                Ok(a) => ParsedCli::Cage(a),
                Err(e) => e.exit(),
            }
        }
        "registry" => {
            if rest.is_empty() {
                eprintln!("error: @registry requires an action");
                eprintln!("usage: isol8 @registry list");
                eprintln!("       isol8 @registry update [NAME]");
                eprintln!("       isol8 @registry install [NAME]");
                eprintln!("       isol8 @registry show <ID>");
                std::process::exit(2);
            }
            let mut reg_argv = vec![OsString::from("isol8")];
            reg_argv.extend_from_slice(rest);
            match RegistryArgs::try_parse_from(&reg_argv) {
                Ok(a) => ParsedCli::Registry(a),
                Err(e) => e.exit(),
            }
        }
        other => {
            eprintln!("error: unknown meta command '@{other}'");
            eprintln!();
            eprintln!("Meta commands (prefix '{META_PREFIX}'):");
            eprintln!("  @init              write a default config file");
            eprintln!("  @profiles-list     list all known profile layers");
            eprintln!("  @profiles-show     dump one layer as TOML");
            eprintln!("  @cage              list / show / new / edit / detect / verify cages");
            eprintln!("  @registry          list / update / install / show recipe registries");
            eprintln!("  @diag              find the grant a confined command needs to launch");
            eprintln!("  @version           print version and exit");
            eprintln!();
            eprintln!("Run 'isol8 --help' for confinement usage.");
            std::process::exit(2);
        }
    }
}

/// Print the full help text to stdout (usage, flags, meta-commands).
pub fn print_help() {
    let _ = ProfileOpts::command().print_help();
    println!();
    println!("Version: {}", version_string());
    println!();
    println!("Run a command confined (no subcommand needed):");
    println!("  isol8 [OPTIONS] <COMMAND> [ARGS]...");
    println!();
    println!("Introspection (dry-run style, no execution):");
    println!("  isol8 --show-policies [OPTIONS] <COMMAND> [ARGS]...");
    println!("  isol8 --show-profiles [OPTIONS]              # list all layers");
    println!("  isol8 --show-profiles [OPTIONS] <COMMAND> ...  # layers selected for command");
    println!();
    println!("Meta commands (prefix '{META_PREFIX}', never passed to the confined process):");
    println!("  isol8 @init [--path DIR] [--format toml|yaml]");
    println!("  isol8 @profiles-list [--verbose] [OPTIONS]");
    println!("  isol8 @profiles-show <NAME> [OPTIONS]");
    println!("  isol8 @cage list|show <NAME>");
    println!("  isol8 @cage new <NAME> [--yes] [--home MODE] [--tools …] [--dir …]");
    println!("  isol8 @cage edit <NAME> [--yes] [--tools …] [--force]");
    println!("  isol8 @cage detect                        # probe toolchains in ~");
    println!("  isol8 @cage verify [NAME]                 # smoke-test a cage");
    println!("  isol8 @registry list|update|install [NAME]  # offline recipe registries");
    println!("  isol8 @registry show <ID>                 # index entry for a recipe id");
    println!("  isol8 @diag [OPTIONS] <COMMAND> [ARGS]...   # why does it abort at launch?");
    println!("  isol8 @version                              # print version and exit");
    println!();
    println!("Cages (named selection units → profiles / home / dirs):");
    println!("  isol8 -c work <COMMAND>...                  # --cage / ISOL8_CAGE / config cage=");
    println!();
    println!("Registries (offline-by-default; configure [registries.*] in isol8.toml):");
    println!(
        "  isol8 @registry update                    # fetch/refresh cache + write isol8.lock"
    );
    println!("  isol8 @registry install                   # show diff, pin lockfile");
    println!();
    println!("Policy diagnosis:");
    println!("  isol8 --analyze <COMMAND>...                # denials → recipe suggestions");
    println!("  isol8 --analyze --author <COMMAND>...       # macOS: Seatbelt trace (permissive)");
    println!("  ISOL8_ANALYZE_FEED=denials.ndjson isol8 --analyze …  # synthetic/offline feed");
}

// ===== CLI entry point and command glue (the `isol8` binary lives here) =====

pub mod config;
pub mod diag;

use anyhow::{bail, Context, Result};
use std::io::Write;

use isol8_core::{backends, profile, resolve, sandbox};

/// Entry point for the `isol8` binary (the `main.rs` shim calls this).
pub fn main() -> Result<()> {
    // Wire offline registry dirs into the core recipe loader. (The config root —
    // `.isol8.toml` config_path / ISOL8_CONFIG_PATH — is resolved by
    // `isol8_core::config` directly; no provider needed.)
    isol8_core::recipe::set_offline_registry_provider(isol8_registry::discover_offline_recipe_dirs);
    match parse() {
        ParsedCli::Help => {
            print_help();
            Ok(())
        }
        ParsedCli::Version => {
            println!("isol8 {}", version_string());
            Ok(())
        }
        ParsedCli::Run(mut run) => {
            prepare_run(&mut run)?;
            run_cmd(run)
        }
        ParsedCli::Init(init) => init_cmd(init),
        ParsedCli::ProfilesList(list) => profiles_list_cmd(list),
        ParsedCli::ProfilesShow(show) => profiles_show_cmd(show),
        ParsedCli::Cage(c) => cage_cmd(c),
        ParsedCli::Registry(r) => registry_cmd(r),
        ParsedCli::Diag(d) => diag_cmd(d),
    }
}

fn registry_cmd(args: RegistryArgs) -> Result<()> {
    use isol8_registry::{
        self, apply_update_to_lockfile, default_cache_root, diff_index, discover_lockfile_path,
        open_offline, update_registry, verify_lock_against_disk, DirSource, Lockfile,
        ProfileSource, RegistrySpec,
    };

    let cfg = config::load()?;
    // Config stores registries as raw TOML tables; type them once here.
    let registries =
        isol8_registry::registries_from_config(&cfg).map_err(|e| anyhow::anyhow!("{e}"))?;
    let lock_path = args
        .lockfile
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(discover_lockfile_path);
    let mut lock = Lockfile::load(&lock_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    let cache_root = default_cache_root();

    let names: Vec<String> = match args.name.as_deref() {
        Some(n)
            if matches!(args.action.as_str(), "update" | "install" | "list")
                && registries.contains_key(n) =>
        {
            vec![n.to_string()]
        }
        Some(_) if args.action == "show" => Vec::new(), // show uses name as id
        None if matches!(args.action.as_str(), "update" | "install" | "list") => {
            registries.keys().cloned().collect()
        }
        _ => registries.keys().cloned().collect(),
    };

    match args.action.as_str() {
        "list" => {
            if registries.is_empty() {
                if args.json {
                    println!("[]");
                    return Ok(());
                }
                println!("# no registries configured");
                println!("# add to isol8.toml:");
                println!("#   [registries.official]");
                println!("#   path = \"/path/to/isol8-recipes\"");
                println!("#   # or: git = \"https://github.com/…/isol8-recipes\"");
                println!("#   #     ref = \"v1\"");
                return Ok(());
            }
            if args.json {
                let rows: Vec<_> = registries
                    .iter()
                    .map(|(name, spec)| {
                        let entry = lock.registry(name);
                        serde_json::json!({
                            "name": name,
                            "trust": entry
                                .and_then(|r| r.trust.clone())
                                .unwrap_or_else(|| spec.default_trust().as_str().to_string()),
                            "pin": entry.map(|r| r.pin.clone()),
                            "source": spec.source_label(),
                            "cached": open_offline(name, spec, &cache_root, &lock).is_ok(),
                        })
                    })
                    .collect();
                println!("{}", to_json(&rows)?);
                return Ok(());
            }
            println!("{:<14} {:<10} {:<8} SOURCE", "NAME", "TRUST", "PINNED");
            for name in registries.keys() {
                let spec = &registries[name];
                let pin = lock
                    .registry(name)
                    .map(|r| {
                        let p = &r.pin;
                        if p.len() > 12 {
                            format!("{}…", &p[..12])
                        } else {
                            p.clone()
                        }
                    })
                    .unwrap_or_else(|| "-".into());
                let trust = lock
                    .registry(name)
                    .and_then(|r| r.trust.clone())
                    .unwrap_or_else(|| spec.default_trust().as_str().to_string());
                println!(
                    "{:<14} {:<10} {:<8} {}",
                    name,
                    trust,
                    pin,
                    spec.source_label()
                );
                // Offline open summary when available.
                if let Ok(src) = open_offline(name, spec, &cache_root, &lock) {
                    println!(
                        "  → {} entries ({}), root {}",
                        src.index().entries.len(),
                        src.trust().as_str(),
                        src.root()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "?".into())
                    );
                } else if matches!(spec, RegistrySpec::Git { .. }) {
                    println!("  → not cached (run: isol8 @registry update {name})");
                }
            }
            if lock_path.is_file() {
                println!();
                println!("lockfile: {}", lock_path.display());
            }
            Ok(())
        }
        "update" => {
            if names.is_empty() {
                bail!("no registries configured — add [registries.*] to isol8.toml");
            }
            for name in &names {
                let spec = registries
                    .get(name)
                    .ok_or_else(|| anyhow::anyhow!("unknown registry '{name}'"))?;
                println!("updating registry '{name}' ({}) …", spec.source_label());
                let upd =
                    update_registry(name, spec, &cache_root).map_err(|e| anyhow::anyhow!("{e}"))?;
                let src = DirSource::open_with_trust(name, &upd.path, Some(upd.trust))
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                println!(
                    "  pin={}  entries={}  trust={}  path={}",
                    if upd.pin.len() > 16 {
                        format!("{}…", &upd.pin[..16])
                    } else {
                        upd.pin.clone()
                    },
                    upd.entry_count,
                    upd.trust.as_str(),
                    upd.path.display()
                );
                if !args.no_lock {
                    apply_update_to_lockfile(&mut lock, &upd, &src);
                }
            }
            if !args.no_lock {
                lock.save(&lock_path).map_err(|e| anyhow::anyhow!("{e}"))?;
                println!("wrote {}", lock_path.display());
            }
            Ok(())
        }
        "install" => {
            // install = update (if needed) + print diff + pin lockfile
            if names.is_empty() {
                bail!("no registries configured — add [registries.*] to isol8.toml");
            }
            let mut hard_fail = false;
            for name in &names {
                let spec = registries
                    .get(name)
                    .ok_or_else(|| anyhow::anyhow!("unknown registry '{name}'"))?;
                // Prefer offline open; fall back to update for git.
                let src = match open_offline(name, spec, &cache_root, &lock) {
                    Ok(s) => s,
                    Err(_) => {
                        println!("fetching '{name}' …");
                        let upd = update_registry(name, spec, &cache_root)
                            .map_err(|e| anyhow::anyhow!("{e}"))?;
                        DirSource::open_with_trust(name, &upd.path, Some(upd.trust))
                            .map_err(|e| anyhow::anyhow!("{e}"))?
                    }
                };
                println!("registry '{name}' — install diff:");
                let items = diff_index(&lock, &src).map_err(|e| anyhow::anyhow!("{e}"))?;
                for it in &items {
                    if it.change == "same" {
                        continue;
                    }
                    println!(
                        "  [{:>7}] {} ({}) {}",
                        it.change, it.id, it.kind, it.summary
                    );
                    for f in &it.flags {
                        let bang = f.contains("FORBIDDEN")
                            || f.contains("ceiling violation")
                            || f.contains("sensitive");
                        if bang {
                            println!("           !! {f}");
                            if args.strict
                                && (f.contains("FORBIDDEN") || f.contains("ceiling violation"))
                            {
                                hard_fail = true;
                            }
                        } else if f.contains("rw on real home") {
                            println!("           +  {f}");
                        } else {
                            println!("              {f}");
                        }
                    }
                }
                let same = items.iter().filter(|i| i.change == "same").count();
                let added = items.iter().filter(|i| i.change == "added").count();
                let changed = items.iter().filter(|i| i.change == "changed").count();
                println!("  summary: {added} added, {changed} changed, {same} unchanged");

                if !args.no_lock {
                    let content_hash = src
                        .index_content_hash()
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    let upd = isol8_registry::UpdateResult {
                        name: name.clone(),
                        pin: lock
                            .registry(name)
                            .map(|r| r.pin.clone())
                            .unwrap_or_else(|| content_hash.clone()),
                        path: src.root().unwrap().to_path_buf(),
                        trust: src.trust(),
                        entry_count: src.index().entries.len(),
                        content_hash,
                        fetched: false,
                    };
                    // Re-pin path registries to current content hash.
                    if matches!(spec, RegistrySpec::Path { .. }) {
                        let upd = isol8_registry::UpdateResult {
                            pin: upd.content_hash.clone(),
                            ..upd
                        };
                        apply_update_to_lockfile(&mut lock, &upd, &src);
                    } else {
                        apply_update_to_lockfile(&mut lock, &upd, &src);
                    }
                }
            }
            if !args.no_lock {
                lock.save(&lock_path).map_err(|e| anyhow::anyhow!("{e}"))?;
                println!("wrote {}", lock_path.display());
            }
            if hard_fail {
                bail!("install aborted (--strict): forbidden path or ceiling violation");
            }
            Ok(())
        }
        "show" => {
            let id = args
                .name
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("@registry show requires a recipe/profile id"))?;
            let mut found = false;
            for (name, spec) in &registries {
                let Ok(src) = open_offline(name, spec, &cache_root, &lock) else {
                    continue;
                };
                if let Some(entry) = src.index().get(id) {
                    found = true;
                    println!("registry: {name}");
                    println!("id:       {}", entry.id);
                    println!("kind:     {:?}", entry.kind);
                    println!("file:     {}", entry.file);
                    println!("summary:  {}", entry.summary);
                    println!("os:       {:?}", entry.os);
                    if !entry.strategies.is_empty() {
                        println!("strategies: {}", entry.strategies.join(", "));
                    }
                    if let Some(ds) = &entry.default_strategy {
                        println!("default:  {ds}");
                    }
                    if let Some(d) = &entry.detects {
                        println!("detects:  {d}");
                    }
                    if let Some(h) = &entry.sha256 {
                        println!("sha256:   {h}");
                    }
                    println!(
                        "trust:    {} (commands {})",
                        src.trust().as_str(),
                        if src.trust().commands_allowed() {
                            "allowed"
                        } else {
                            "blocked"
                        }
                    );
                }
            }
            if !found {
                // Also search merged offline recipe registry.
                let reg = isol8_core::recipe::RecipeRegistry::load(&[])
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                if reg.ids().iter().any(|i| i == id) {
                    println!("id: {id}");
                    println!("source: loaded in RecipeRegistry (builtin/user/registry cache)");
                    found = true;
                }
            }
            if !found {
                bail!("no index entry for '{id}' in configured registries");
            }
            Ok(())
        }
        "verify" => {
            // Optional: check lockfile drift.
            let drifts = verify_lock_against_disk(&registries, &cache_root, &lock)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if drifts.is_empty() {
                println!("lockfile ok ({})", lock_path.display());
            } else {
                for d in &drifts {
                    println!("drift: {d}");
                }
                bail!("{} lockfile drift error(s)", drifts.len());
            }
            Ok(())
        }
        other => bail!(
            "unknown @registry action '{other}' (expected list, update, install, show, verify)"
        ),
    }
}

fn diag_cmd(d: DiagArgs) -> Result<()> {
    let mut run = RunInvocation {
        opts: d.opts,
        cmd: d.cmd,
    };
    prepare_run(&mut run)?;
    if run.cmd.is_empty() {
        bail!("@diag needs a command (e.g. isol8 @diag node --version)");
    }
    let args = run_from(run.opts, run.cmd);
    diag::run(&args)
}

fn prepare_run(run: &mut RunInvocation) -> Result<()> {
    prepare_opts(&mut run.opts)
}

/// Resolve config → env → cage into `opts`, leaving CLI-set fields untouched.
///
/// Precedence ([`_docs/config.md`](../../../../_docs/config.md) §7): builtin
/// defaults → config file (+ project marker overlay) → `ISOL8_*` → CLI flags →
/// cage (fills what is still empty). The cage is applied before the config
/// defaults because it is the more specific selection.
fn prepare_opts(opts: &mut ProfileOpts) -> Result<()> {
    let cli_auto = opts.auto_profiles_cli_override();
    let mut cfg = config::load()?;
    // Env overrides the config, not the command line.
    config::apply_env_overrides(&mut cfg);

    apply_cage_to_opts(opts, &cfg)?;
    config::apply_to_run(&cfg, opts, cli_auto);
    if let Some(v) = cli_auto {
        opts.auto_profiles = v;
    }
    Ok(())
}

/// Effective isol8 config root (`ISOL8_CONFIG_PATH` / project `config_path` / OS).
fn active_config_root() -> std::path::PathBuf {
    isol8_registry::effective_config_dir()
}

/// Run toolchain detect; on a TTY, rewrite stderr with `3/22 checking nvm…`.
fn detect_with_progress(
    reg: &isol8_core::recipe::RecipeRegistry,
    ctx: &isol8_core::filter::RunContext,
    real: &std::path::Path,
) -> Result<Vec<isol8_core::detect::DetectResult>> {
    use std::io::Write;

    if !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return isol8_core::detect::detect_all(reg, ctx, real).map_err(Into::into);
    }
    let mut progress = |i: usize, total: usize, name: &str| {
        eprint!("\r\x1b[K{i}/{total} checking {name}...");
        let _ = std::io::stderr().flush();
    };
    let rows = isol8_core::detect::detect_all_with(reg, ctx, real, Some(&mut progress))?;
    // Clear the progress line before the final table.
    eprint!("\r\x1b[K");
    let _ = std::io::stderr().flush();
    Ok(rows)
}

/// Resolve cage selection and merge into opts. CLI-set fields are never overwritten.
///
/// Name resolution and the field merge both live in `isol8_core::cage`; this only
/// bridges the clap-derived [`ProfileOpts`] to the engine's [`Spec`].
fn apply_cage_to_opts(opts: &mut ProfileOpts, cfg: &config::Config) -> Result<()> {
    let name = isol8_core::cage::select_name(opts.cage.as_deref(), cfg);
    let cwd = std::env::current_dir().context("resolving current directory for cage discovery")?;
    let config_root = active_config_root();
    let Some(cage) = isol8_core::cage::resolve_in(name.as_deref(), &cwd, Some(&config_root))?
    else {
        return Ok(());
    };

    // Round-trip through Spec so the merge rule has exactly one implementation.
    let mut spec = opts.clone().into_spec(Vec::new());
    isol8_core::cage::apply_overlay(&cage.overlay(), &mut spec);
    opts.profiles = spec.profiles;
    opts.home = spec.home;
    opts.ephemeral_home = spec.ephemeral_home;
    opts.add_dirs_rw = spec.add_dirs_rw;
    opts.add_dirs_ro = spec.add_dirs_ro;
    opts.toolchains = spec.toolchains;
    Ok(())
}

fn cage_cmd(args: CageArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let config_root = active_config_root();
    match args.action.as_str() {
        "list" => {
            let list = isol8_core::cage::list_cages_in(&cwd, Some(&config_root))?;
            if list.is_empty() {
                println!("(no cages found)");
                let dir = isol8_core::cage::cages_dir(Some(&config_root))
                    .unwrap_or_else(|| config_root.join("cages"));
                println!("# create one: isol8 @cage new <name> --yes");
                println!("# cages dir:  {}", dir.display());
                return Ok(());
            }
            if args.json {
                let rows: Vec<_> = list
                    .iter()
                    .map(|(name, path)| serde_json::json!({ "name": name, "path": path }))
                    .collect();
                println!("{}", to_json(&rows)?);
                return Ok(());
            }
            for (name, path) in list {
                println!("{name}\t{}", path.display());
            }
            Ok(())
        }
        "show" => {
            let name = args
                .name
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("@cage show requires a name"))?;
            let cage = isol8_core::cage::resolve_in(Some(name), &cwd, Some(&config_root))?
                .ok_or_else(|| anyhow::anyhow!("cage '{name}' not found"))?;
            if args.json {
                println!("{}", to_json(&cage)?);
                return Ok(());
            }
            print!("{}", isol8_core::cage::format_show(&cage));
            Ok(())
        }
        "new" => cage_new_cmd(args, &cwd, false),
        "edit" => cage_new_cmd(args, &cwd, true),
        "detect" => cage_detect_cmd(args.json),
        "verify" => cage_verify_cmd(args.name.as_deref(), args.json),
        other => {
            bail!(
                "unknown @cage action '{other}' \
                 (expected list, show, new, edit, detect, or verify)"
            );
        }
    }
}

/// Shared implementation for `@cage new` and `@cage edit`.
fn cage_new_cmd(args: CageArgs, cwd: &std::path::Path, is_edit: bool) -> Result<()> {
    use crate::wizard::{self, WizardRequest};
    use isol8_core::profile::Access;
    use isol8_core::recipe::RecipeRegistry;

    let name = args
        .name
        .as_deref()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "@cage {} requires a name",
                if is_edit { "edit" } else { "new" }
            )
        })?
        .to_string();

    let reg = RecipeRegistry::load(&[])?;
    let run_ctx = isol8_core::filter::RunContext::from_cmd(&[]);
    let real = isol8_core::context::real_home_from_env();

    // Always show detection first (evo-repo §6.2) — even for --yes.
    // TTY: rewrite stderr with `3/22 checking nvm…` while probes run.
    let detected = detect_with_progress(&reg, &run_ctx, &real)?;
    print!("{}", isol8_core::detect::format_detect_table(&detected));

    let config_root = active_config_root();
    let existing = if is_edit {
        let cage = isol8_core::cage::resolve_in(Some(&name), cwd, Some(&config_root))?
            .ok_or_else(|| anyhow::anyhow!("cage '{name}' not found"))?;
        Some(cage)
    } else {
        None
    };

    let interactive = !args.yes
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stdout());

    // Bundle seed
    let mut home = args.home.clone();
    let mut tools = Vec::new();
    let mut profiles: Vec<String> = args
        .profiles
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(|x| x.to_string())
                .collect()
        })
        .unwrap_or_default();
    let mut dirs: Vec<(String, Access)> =
        args.dirs.iter().map(|d| (d.clone(), Access::Rw)).collect();

    if let Some(from) = &args.from {
        let bundle = wizard::expand_bundle(from).map_err(|e| anyhow::anyhow!("{e}"))?;
        println!("# seeded from bundle {}", bundle.id);
        if home == "inherit" {
            if let Some(h) = &bundle.home {
                // Bundle homes like @managed/polyglot-agent — if literal managed
                // id differs from cage name, keep bundle value unless user set managed.
                home = if h.starts_with("@managed/") {
                    "managed".into() // normalize to this cage's managed id
                } else {
                    h.clone()
                };
            }
        }
        if profiles.is_empty() {
            profiles = bundle.profiles;
        }
        tools = bundle.tools;
    }

    if let Some(t) = &args.tools {
        tools = wizard::parse_tools_list(t, Some(&reg)).map_err(|e| anyhow::anyhow!("{e}"))?;
    } else if tools.is_empty() && (args.yes || !interactive) {
        // Non-interactive default: all found toolchains with recipe defaults.
        let found: Vec<String> = detected
            .iter()
            .filter(|d| d.found)
            .map(|d| d.id.clone())
            .collect();
        tools = wizard::tools_from_detect(&reg, &found).map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    // Edit: fill gaps from the existing cage before interactive / write.
    if is_edit {
        if let Some(cage) = &existing {
            if args.home == "inherit" && args.from.is_none() {
                home = match &cage.home {
                    isol8_core::cage::HomeMode::Inherit => "inherit".into(),
                    isol8_core::cage::HomeMode::Ephemeral => "ephemeral".into(),
                    isol8_core::cage::HomeMode::Path(p) => p.clone(),
                };
            }
            if args.tools.is_none() && args.from.is_none() {
                tools = cage.toolchains.clone();
            }
            if dirs.is_empty() {
                for d in &cage.dirs {
                    dirs.push((d.path.clone(), d.access));
                }
            }
            if profiles.is_empty() {
                profiles = cage.profiles.clone();
            }
        }
    }

    if interactive {
        let picks = cage_wizard_interactive(InteractiveOpts {
            name: &name,
            home_default: &home,
            detected: &detected,
            reg: &reg,
            is_edit,
            existing: existing.as_ref(),
            keep_tools: args.tools.is_some() || args.from.is_some(),
            preset_tools: &tools,
        })?;
        home = picks.home;
        if !picks.keep_tools {
            tools = picks.tools;
        }
        if let Some(d) = picks.dir {
            if args.dirs.is_empty() && !dirs.iter().any(|(p, _)| p == &d) {
                dirs.push((d, Access::Rw));
            }
        }
        if !picks.profiles.is_empty() && args.profiles.is_none() {
            profiles = picks.profiles;
        }
    } else if !args.yes && !args.preview {
        bail!(
            "non-interactive terminal: pass --yes (and optional --tools / --home) \
             or run in a TTY for prompts"
        );
    }

    let out_dir = args.path.as_ref().map(PathBuf::from);
    let existing_path = existing.as_ref().map(|c| c.source.clone());

    let req = WizardRequest {
        name: name.clone(),
        home,
        tools: tools.clone(),
        dirs,
        profiles,
        out_dir,
        force: args.force,
        existing_path: existing_path.clone(),
    };

    let rendered = wizard::render(&req).map_err(|e| anyhow::anyhow!("{e}"))?;

    let notes = wizard::preview_security_notes(&tools, &reg);
    if !notes.is_empty() {
        println!("# security-relevant grants (rw outside replaced home):");
        for n in &notes {
            println!("#   !! {n}");
        }
    }
    for w in &rendered.warnings {
        eprintln!("warning: {w}");
    }

    if args.preview {
        println!("# preview → {}", rendered.path.display());
        if let Ok(ctx) = isol8_core::context::Context::from_environment() {
            let home_token =
                wizard::normalize_home(&name, &req.home).map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Ok(desc) = ctx.describe_home(&home_token) {
                println!("# home: {desc}");
            }
        }
        print!("{}", rendered.body);
        return Ok(());
    }

    if interactive {
        use dialoguer::Confirm;
        println!("# will write {}", rendered.path.display());
        if let Ok(ctx) = isol8_core::context::Context::from_environment() {
            let home_token =
                wizard::normalize_home(&name, &req.home).map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Ok(desc) = ctx.describe_home(&home_token) {
                println!("# home: {desc}");
            }
        }
        println!("{}", rendered.body);
        if !Confirm::new()
            .with_prompt("Write this cage?")
            .default(true)
            .interact()?
        {
            println!("aborted");
            return Ok(());
        }
    }

    let state = wizard::state_path();
    let result = wizard::apply(&req, &state).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("wrote {}", result.path.display());
    // Resolve home token → on-disk path so users see where $HOME will live.
    let home_token =
        wizard::normalize_home(&name, &req.home).map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Ok(ctx) = isol8_core::context::Context::from_environment() {
        if let Ok(desc) = ctx.describe_home(&home_token) {
            println!("# home: {desc}");
        }
    }
    let hash_short = &result.managed_hash[..12.min(result.managed_hash.len())];
    println!("# managed hash {hash_short}");

    if args.verify {
        cage_verify_cmd(Some(&name), false)?;
    } else {
        println!("# next: isol8 @cage verify {name}");
        println!("# run:  isol8 -c {name} <cmd>…");
    }
    Ok(())
}

struct InteractivePicks {
    home: String,
    tools: Vec<isol8_core::recipe::ToolchainChoice>,
    /// When true, caller should keep the pre-parsed tools list.
    keep_tools: bool,
    dir: Option<String>,
    profiles: Vec<String>,
}

struct InteractiveOpts<'a> {
    name: &'a str,
    home_default: &'a str,
    detected: &'a [isol8_core::detect::DetectResult],
    reg: &'a isol8_core::recipe::RecipeRegistry,
    is_edit: bool,
    existing: Option<&'a isol8_core::cage::Cage>,
    keep_tools: bool,
    preset_tools: &'a [isol8_core::recipe::ToolchainChoice],
}

fn cage_wizard_interactive(opts: InteractiveOpts<'_>) -> Result<InteractivePicks> {
    use crate::wizard::{self, default_strategy_for};
    use dialoguer::{Confirm, Input, MultiSelect, Select};
    use isol8_core::recipe::{StrategyName, ToolchainChoice};

    let InteractiveOpts {
        name,
        home_default,
        detected,
        reg,
        is_edit,
        existing,
        keep_tools,
        preset_tools,
    } = opts;

    println!();
    println!(
        "Cage wizard ({}) — {}",
        name,
        if is_edit { "edit" } else { "new" }
    );

    // 1. Home
    let home_options = [
        "inherit — keep real $HOME (no replacement)",
        "managed — isol8-managed home (@managed/<name>)",
        "ephemeral — fresh tmpdir each run",
        "custom path…",
    ];
    let default_home_idx = match home_default {
        "inherit" => 0,
        "managed" => 1,
        "ephemeral" => 2,
        h if h.starts_with("@managed/") => 1,
        _ => 3,
    };
    let home_idx = Select::new()
        .with_prompt("Home mode")
        .items(&home_options)
        .default(default_home_idx)
        .interact()?;
    let home = match home_idx {
        0 => "inherit".to_string(),
        1 => "managed".to_string(),
        2 => "ephemeral".to_string(),
        _ => {
            let initial = if !matches!(home_default, "inherit" | "managed" | "ephemeral") {
                home_default
            } else {
                ""
            };
            Input::new()
                .with_prompt("Home path")
                .with_initial_text(initial)
                .interact_text()?
        }
    };

    // 2. Toolchains — multi-select unless --tools/--from already set
    let tools = if keep_tools {
        preset_tools.to_vec()
    } else {
        let mut labels: Vec<String> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        let mut defaults: Vec<bool> = Vec::new();
        let existing_ids: std::collections::HashSet<String> = existing
            .map(|c| c.toolchains.iter().map(|t| t.id.clone()).collect())
            .unwrap_or_default();

        for d in detected {
            let short = d.id.strip_prefix("toolchains/").unwrap_or(&d.id);
            let mark = if d.found { "found" } else { "absent" };
            let strat_hint = reg
                .resolve(&d.id, &isol8_core::filter::RunContext::from_cmd(&[]))
                .map(|r| default_strategy_for(r).as_str())
                .unwrap_or("link");
            labels.push(format!("{short} [{mark}] → {strat_hint}"));
            ids.push(d.id.clone());
            defaults.push(if is_edit {
                existing_ids.contains(&d.id)
            } else {
                d.found
            });
        }

        if labels.is_empty() {
            Vec::new()
        } else {
            let chosen = MultiSelect::new()
                .with_prompt("Toolchains (space to toggle, enter to confirm)")
                .items(&labels)
                .defaults(&defaults)
                .interact()?;
            let mut out = Vec::new();
            for i in chosen {
                let id = &ids[i];
                let strategy = if let Ok(recipe) =
                    reg.resolve(id, &isol8_core::filter::RunContext::from_cmd(&[]))
                {
                    let def = default_strategy_for(recipe);
                    let opts: Vec<&str> = ["link", "share", "isolate"]
                        .into_iter()
                        .filter(|s| {
                            StrategyName::parse(s)
                                .ok()
                                .is_some_and(|sn| recipe.strategies.contains_key(&sn))
                        })
                        .collect();
                    if opts.len() <= 1 {
                        def
                    } else if Confirm::new()
                        .with_prompt(format!(
                            "  override strategy for {}? (default {})",
                            id.strip_prefix("toolchains/").unwrap_or(id),
                            def.as_str()
                        ))
                        .default(false)
                        .interact()?
                    {
                        let di = opts.iter().position(|s| *s == def.as_str()).unwrap_or(0);
                        let si = Select::new()
                            .with_prompt("  strategy")
                            .items(&opts)
                            .default(di)
                            .interact()?;
                        StrategyName::parse(opts[si])?
                    } else {
                        def
                    }
                } else {
                    StrategyName::Link
                };
                out.push(ToolchainChoice {
                    id: id.clone(),
                    strategy,
                });
            }
            out
        }
    };

    // 3. Project dirs
    let cwd = std::env::current_dir().unwrap_or_default();
    let cwd_s = cwd.display().to_string();
    let dir = if Confirm::new()
        .with_prompt(format!("Grant rw on current directory ({cwd_s})?"))
        .default(true)
        .interact()?
    {
        Some(cwd_s)
    } else {
        None
    };

    wizard::normalize_home(name, &home).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(InteractivePicks {
        home,
        tools,
        keep_tools,
        dir,
        profiles: existing.map(|c| c.profiles.clone()).unwrap_or_default(),
    })
}

fn cage_detect_cmd(json: bool) -> Result<()> {
    let reg = isol8_core::recipe::RecipeRegistry::load(&[])?;
    let ctx = isol8_core::filter::RunContext::from_cmd(&[]);
    let real = isol8_core::context::real_home_from_env();
    let results = isol8_core::detect::detect_all(&reg, &ctx, &real)?;
    if json {
        println!("{}", to_json(&results)?);
        return Ok(());
    }
    print!("{}", isol8_core::detect::format_detect_table(&results));
    Ok(())
}

fn cage_verify_cmd(name: Option<&str>, json: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let config_root = active_config_root();
    let cage = isol8_core::cage::resolve_in(name, &cwd, Some(&config_root))?.ok_or_else(|| {
        if let Some(n) = name {
            anyhow::anyhow!("cage '{n}' not found")
        } else {
            anyhow::anyhow!("no default cage found (pass a name: isol8 @cage verify <NAME>)")
        }
    })?;

    let overlay = cage.overlay();
    let cfg = config::load()?;
    let mut spec = isol8_core::sandbox::Spec::new(["true"]);
    isol8_core::cage::apply_overlay(&overlay, &mut spec);
    if spec.profiles.is_empty() {
        spec.profiles = cfg.default_profiles.clone();
    }

    if !json {
        println!("Verifying cage '{}' ({})", cage.name, cage.source.display());
    }
    let results = isol8_core::detect::verify_toolchains(&spec)?;
    if json {
        println!("{}", to_json(&results)?);
    } else {
        print!("{}", isol8_core::detect::format_verify_report(&results));
    }
    if results.iter().any(|r| !r.ok) {
        std::process::exit(1);
    }
    Ok(())
}

fn run_cmd(run: RunInvocation) -> Result<()> {
    if run.show_policies() {
        if run.cmd.is_empty() {
            bail!("--show-policies requires a command (e.g. isol8 --show-policies -- echo hi)");
        }
        let json = run.opts.json;
        let args = run_from(run.opts, run.cmd);
        let dry = sandbox::dry_run(&args)?;
        if json {
            println!("{}", to_json(&dry)?);
        } else {
            print_dry_run(&dry);
        }
        return Ok(());
    }

    if run.show_profiles() {
        if run.cmd.is_empty() {
            return profiles_list(registry_from_run(&run)?, run.verbose());
        }
        let json = run.opts.json;
        let args = run_from(run.opts, run.cmd);
        let effective = resolve::effective_policy(&args)?;
        if json {
            let rows: Vec<_> = effective
                .layer_names
                .iter()
                .map(|(name, origin)| serde_json::json!({ "name": name, "origin": origin.label() }))
                .collect();
            println!("{}", to_json(&rows)?);
            return Ok(());
        }
        println!("== selected layers ==");
        for (name, origin) in &effective.layer_names {
            println!("  {name} ({})", origin.label());
        }
        return Ok(());
    }

    if run.cmd.is_empty() {
        print_help();
        return Ok(());
    }

    if run.opts.analyze {
        return analyze_cmd(run);
    }

    sandbox::ensure_not_nested()?;

    let args = run_from(run.opts, run.cmd);
    let mut effective = resolve::effective_policy(&args)?;

    isol8_core::home::materialize(&effective.home)?;
    resolve::confine_executable(&mut effective.profile, &mut effective.cmd)?;

    let backend = backends::select();
    let mut child = backend.spawn(&effective.profile, &effective.env, &effective.cmd)?;
    let code = child.wait()?;
    std::process::exit(code);
}

/// `--analyze`: run the command (best-effort), load denials, print suggestions.
fn analyze_cmd(run: RunInvocation) -> Result<()> {
    let author = run.opts.author;
    let json = run.opts.json;
    if author && !run.opts.analyze {
        bail!("--author requires --analyze (explicit opt-in for Seatbelt trace mode)");
    }
    if author && !cfg!(target_os = "macos") {
        eprintln!("warning: --author is only implemented on macOS (Seatbelt trace); ignoring");
    }

    let mut opts = isol8_core::analyze::AnalyzeOptions::default();
    if author && cfg!(target_os = "macos") {
        opts.author_trace = Some(isol8_core::analyze::default_trace_path());
        eprintln!(
            "warning: --author enables Seatbelt (trace …) — permissive observation, not production policy"
        );
    }

    let args = run_from(run.opts, run.cmd);
    let ctx = isol8_core::context::Context::from_environment()?;
    let outcome = isol8_core::analyze::run_and_analyze_with(&args, &ctx, &opts)?;

    if json {
        println!("{}", to_json(&outcome)?);
        return Ok(());
    }

    println!("== isol8 --analyze ==");
    println!("command exit code: {}", outcome.code);
    print!("{}", outcome.report.render());
    if let Some(p) = &opts.author_trace {
        if p.is_file() {
            println!("\n--author trace profile written to {}", p.display());
            println!(
                "  (Seatbelt-generated allow list of paths actually touched; review before use)"
            );
        } else {
            println!(
                "\n--author: no trace file at {} (command may have exited before Seatbelt flushed)",
                p.display()
            );
        }
    }
    Ok(())
}

/// Render any serializable engine value as pretty JSON (`--json`).
fn to_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value).context("serializing --json output")
}

fn registry_from_run(run: &RunInvocation) -> Result<profile::LayerRegistry> {
    Ok(profile::LayerRegistry::load(run.profile_paths())?)
}

/// Render a structured [`sandbox::DryRun`] as the `--show-policies` text report:
/// the layer stack (with provenance), merged path grants, sanitized env, the target
/// command, and the OS-native policy text.
fn print_dry_run(dry: &sandbox::DryRun) {
    println!("== layer stack ==");
    for (name, origin) in &dry.layer_names {
        println!("  {name} ({})", origin.label());
    }

    println!("== isol8 effective policy (dry-run) ==");

    println!("\n-- home --");
    println!("  path = {}", dry.home_path.display());
    println!("  materialization plan:");
    print!("{}", dry.home_plan.render());

    if !dry.recipes.is_empty() {
        println!("\n-- recipes --");
        for (id, strategy) in &dry.recipes {
            println!("  {id}  strategy={strategy}");
        }
    }

    println!("\n-- path grants --");
    if dry.profile.paths.is_empty() {
        println!("  (none — deny-by-default; nothing is reachable)");
    } else {
        for g in &dry.profile.paths {
            println!(
                "  {:<8} {:<8} {}",
                format!("{:?}", g.access).to_lowercase(),
                format!("{:?}", g.r#match).to_lowercase(),
                g.path
            );
        }
    }

    println!("\n-- environment --");
    let mut keys: Vec<&String> = dry.env.keys().collect();
    keys.sort();
    let home = dry.env.get("HOME").map(String::as_str).unwrap_or("(unset)");
    println!("  HOME = {home}");
    if keys.is_empty() {
        println!("  (empty)");
    } else {
        for k in keys {
            if k == "HOME" {
                continue; // already printed first
            }
            println!("  {k} = {}", dry.env[k]);
        }
    }

    println!("\n-- command --");
    if dry.cmd.is_empty() {
        println!("  (none)");
    } else {
        println!("  {}", dry.cmd.join(" "));
    }

    println!("\n-- {} --", dry.policy_label);
    print!("{}", dry.policy);
}

fn init_cmd(init: InitArgs) -> Result<()> {
    let format = match init.format {
        ConfigFormat::Toml => "toml",
        ConfigFormat::Yaml => "yaml",
    };
    let path = init
        .path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config::default_init_path(format));
    if path.exists() {
        bail!(
            "config already exists at {} (refusing to overwrite)",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating config directory {}", parent.display()))?;
    }
    let body = config::init_template(format)?;
    let mut file = std::fs::File::create(&path)
        .with_context(|| format!("creating config file {}", path.display()))?;
    file.write_all(body.as_bytes())?;
    println!("wrote {}", path.display());
    Ok(())
}

fn profiles_list_cmd(list: ProfilesListArgs) -> Result<()> {
    let mut opts = list.opts;
    prepare_opts(&mut opts)?;
    let registry = profile::LayerRegistry::load(opts.profile_paths.as_slice())?;
    if opts.json {
        let rows: Vec<_> = registry
            .list()
            .into_iter()
            .map(|(name, source)| {
                let p = registry.get(&name);
                serde_json::json!({
                    "name": name,
                    "source": format!("{source:?}"),
                    "requires": p.map(|p| p.requires.clone()).unwrap_or_default(),
                    "policies": p.map(|p| p.policies.len()).unwrap_or(0),
                })
            })
            .collect();
        println!("{}", to_json(&rows)?);
        return Ok(());
    }
    profiles_list(registry, opts.verbose)
}

fn profiles_list(registry: profile::LayerRegistry, verbose: bool) -> Result<()> {
    for (name, source) in registry.list() {
        if verbose {
            if let Some(p) = registry.get(&name) {
                let filt = p
                    .filter
                    .as_ref()
                    .map(|f| format!("{f:?}"))
                    .unwrap_or_else(|| "none".into());
                println!(
                    "{name}\trequires={:?}\tfilter={filt}\tpolicies={}\tsource={source:?}",
                    p.requires,
                    p.policies.len()
                );
            }
        } else {
            println!("{name}\t{source:?}");
        }
    }
    Ok(())
}

fn profiles_show_cmd(mut show: ProfilesShowArgs) -> Result<()> {
    prepare_opts(&mut show.opts)?;
    let registry = profile::LayerRegistry::load(show.opts.profile_paths.as_slice())?;
    let Some(p) = registry.get(&show.name) else {
        bail!("unknown profile '{}'", show.name);
    };
    let src = registry
        .source(&show.name)
        .map(|s| format!("{s:?}"))
        .unwrap_or_default();
    println!("# source: {src}");
    print!("{}", profile::format_layer(p)?);
    Ok(())
}
