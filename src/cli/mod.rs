use clap::{CommandFactory, Parser, ValueEnum};
use std::ffi::OsString;

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

    /// Replacement $HOME (defaults to an auto scratch home when a profile enables it).
    #[arg(long)]
    pub home: Option<String>,

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
    /// Convert parsed CLI options + command into the clap-free engine [`Spec`](crate::sandbox::Spec).
    pub fn into_spec(self, cmd: Vec<String>) -> crate::sandbox::Spec {
        crate::sandbox::Spec {
            profiles: self.profiles,
            profile_paths: self.profile_paths,
            auto_profiles: self.auto_profiles,
            add_dirs_rw: self.add_dirs_rw,
            add_dirs_ro: self.add_dirs_ro,
            cwd_ro: self.cwd_ro,
            home: self.home,
            no_seed: self.no_seed,
            env_pass: self.env_pass,
            set_env: self.set_env,
            cmd,
        }
    }
}

/// Build the engine [`Spec`](crate::sandbox::Spec) from options + command (CLI / test convenience).
pub fn run_from(opts: ProfileOpts, cmd: Vec<String>) -> crate::sandbox::Spec {
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
        other => {
            eprintln!("error: unknown meta command '@{other}'");
            eprintln!();
            eprintln!("Meta commands (prefix '{META_PREFIX}'):");
            eprintln!("  @init              write a default config file");
            eprintln!("  @profiles-list     list all known profile layers");
            eprintln!("  @profiles-show     dump one layer as TOML");
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
    println!("  isol8 @diag [OPTIONS] <COMMAND> [ARGS]...   # why does it abort at launch?");
    println!("  isol8 @version                              # print version and exit");
}

// ===== CLI entry point and command glue (the `isol8` binary lives here) =====

mod config;
mod diag;

use anyhow::{bail, Context, Result};
use std::io::Write;

use crate::{backends, profile, resolve, sandbox};

/// Entry point for the `isol8` binary (the `main.rs` shim calls this).
pub fn main() -> Result<()> {
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
        ParsedCli::Diag(d) => diag_cmd(d),
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
    let cli_auto = run.opts.auto_profiles_cli_override();
    let cfg = config::load()?;
    config::apply_to_run(&cfg, &mut run.opts, cli_auto);
    config::apply_env_overrides(&mut run.opts, cli_auto.is_some());
    if let Some(v) = cli_auto {
        run.opts.auto_profiles = v;
    }
    Ok(())
}

fn prepare_opts(opts: &mut ProfileOpts) -> Result<()> {
    let cli_auto = opts.auto_profiles_cli_override();
    let cfg = config::load()?;
    config::apply_to_run(&cfg, opts, cli_auto);
    config::apply_env_overrides(opts, cli_auto.is_some());
    if let Some(v) = cli_auto {
        opts.auto_profiles = v;
    }
    Ok(())
}

fn run_cmd(run: RunInvocation) -> Result<()> {
    if run.show_policies() {
        if run.cmd.is_empty() {
            bail!("--show-policies requires a command (e.g. isol8 --show-policies -- echo hi)");
        }
        let args = run_from(run.opts, run.cmd);
        let dry = sandbox::dry_run(&args)?;
        print_dry_run(&dry);
        return Ok(());
    }

    if run.show_profiles() {
        if run.cmd.is_empty() {
            return profiles_list(registry_from_run(&run)?, run.verbose());
        }
        let args = run_from(run.opts, run.cmd);
        let effective = resolve::effective_policy(&args)?;
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

    sandbox::ensure_not_nested()?;

    let args = run_from(run.opts, run.cmd);
    let mut effective = resolve::effective_policy(&args)?;

    crate::home::seed(&effective.home)?;
    resolve::confine_executable(&mut effective.profile, &mut effective.cmd)?;

    let backend = backends::select();
    let mut child = backend.spawn(&effective.profile, &effective.env, &effective.cmd)?;
    let code = child.wait()?;
    std::process::exit(code);
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
