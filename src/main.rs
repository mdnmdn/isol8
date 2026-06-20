mod backends;
mod cli;
mod env;
mod profile;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let args = cli::Cli::parse();
    match args.command {
        cli::Command::Run(run) => run_cmd(run),
    }
}

fn run_cmd(run: cli::RunArgs) -> Result<()> {
    // 1. load + merge profile layers, fold in --add-dirs / --home overrides
    let profile = profile::load(&run)?;

    // 2. build sanitized env (HOME resolved FIRST, per R4)
    let env = env::build_minimal(&profile, run.home.as_deref());

    // 3. pick backend for this OS and apply policy
    let backend = backends::select();

    if run.dry_run {
        backends::render_dry_run(&profile, &env, &run.cmd);
        return Ok(());
    }

    let code = backend.spawn(&profile, &env, &run.cmd)?;
    std::process::exit(code);
}
