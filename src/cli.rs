use clap::{CommandFactory, Parser, ValueEnum};
use std::ffi::OsString;

/// Prefix for meta subcommands (not passed to the confined process).
pub const META_PREFIX: &str = "@";

#[derive(Parser, Clone, Default)]
#[command(
    name = "isol8",
    about = "Deny-by-default sandbox for agents and CLI tools",
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

    /// Replacement $HOME (defaults to an auto scratch home when a profile enables it).
    #[arg(long)]
    pub home: Option<String>,

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

#[derive(Parser)]
pub struct RunInvocation {
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
    pub fn profiles(&self) -> &[String] {
        &self.opts.profiles
    }
    pub fn profile_paths(&self) -> &[String] {
        &self.opts.profile_paths
    }
    pub fn auto_profiles(&self) -> bool {
        self.opts.auto_profiles
    }
    pub fn add_dirs_rw(&self) -> &[String] {
        &self.opts.add_dirs_rw
    }
    pub fn add_dirs_ro(&self) -> &[String] {
        &self.opts.add_dirs_ro
    }
    pub fn home(&self) -> &Option<String> {
        &self.opts.home
    }

    pub fn show_policies(&self) -> bool {
        self.opts.show_policies || self.opts.dry_run
    }

    pub fn show_profiles(&self) -> bool {
        self.opts.show_profiles
    }

    pub fn verbose(&self) -> bool {
        self.opts.verbose
    }
}

/// Backward-compatible wrapper used by resolve pipeline.
#[derive(Clone)]
pub struct RunArgs {
    pub opts: ProfileOpts,
    pub cmd: Vec<String>,
}

impl RunArgs {
    pub fn profiles(&self) -> &[String] {
        &self.opts.profiles
    }
    pub fn profile_paths(&self) -> &[String] {
        &self.opts.profile_paths
    }
    pub fn auto_profiles(&self) -> bool {
        self.opts.auto_profiles
    }
    pub fn add_dirs_rw(&self) -> &[String] {
        &self.opts.add_dirs_rw
    }
    pub fn add_dirs_ro(&self) -> &[String] {
        &self.opts.add_dirs_ro
    }
    pub fn home(&self) -> &Option<String> {
        &self.opts.home
    }
    pub fn dry_run(&self) -> bool {
        self.opts.show_policies || self.opts.dry_run
    }
}

pub fn run_from(opts: ProfileOpts, cmd: Vec<String>) -> RunArgs {
    RunArgs { opts, cmd }
}

#[derive(Parser)]
pub struct InitArgs {
    /// Directory or file path for the config (default: OS config dir / isol8.toml).
    #[arg(long)]
    pub path: Option<String>,

    #[arg(long, default_value = "toml")]
    pub format: ConfigFormat,
}

#[derive(Clone, Copy, ValueEnum, Default)]
pub enum ConfigFormat {
    #[default]
    Toml,
    Yaml,
}

#[derive(Parser)]
pub struct ProfilesListArgs {
    #[command(flatten)]
    pub opts: ProfileOpts,
}

#[derive(Parser)]
pub struct ProfilesShowArgs {
    pub name: String,

    #[command(flatten)]
    pub opts: ProfileOpts,
}

/// Top-level parse result.
pub enum ParsedCli {
    /// No arguments — print help.
    Help,
    /// Confine and run (or introspect via --show-* flags).
    Run(RunInvocation),
    Init(InitArgs),
    ProfilesList(ProfilesListArgs),
    ProfilesShow(ProfilesShowArgs),
}

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
            eprintln!();
            eprintln!("Run 'isol8 --help' for confinement usage.");
            std::process::exit(2);
        }
    }
}

pub fn print_help() {
    let _ = ProfileOpts::command().print_help();
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
}
