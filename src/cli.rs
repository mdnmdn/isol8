use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "isol8", about = "Deny-by-default sandbox for agents and CLI tools")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run a command inside the sandbox.
    Run(RunArgs),
}

#[derive(Parser)]
pub struct RunArgs {
    /// Named profile layer to enable (repeatable, deny-first merge order).
    #[arg(long = "profile")]
    pub profiles: Vec<String>,

    /// Grant read-write access to a path (repeatable).
    #[arg(long = "add-dirs-rw")]
    pub add_dirs_rw: Vec<String>,

    /// Grant read-only access to a path (repeatable).
    #[arg(long = "add-dirs-ro")]
    pub add_dirs_ro: Vec<String>,

    /// Replacement $HOME (defaults to an auto scratch home when a profile enables it).
    #[arg(long)]
    pub home: Option<String>,

    /// Print the effective policy without running anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Command and arguments to run, confined.
    #[arg(trailing_var_arg = true, required = true)]
    pub cmd: Vec<String>,
}
