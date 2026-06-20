use anyhow::Result;
use clap::Parser;

use isol8::{backends, cli, env, home, profile};

fn main() -> Result<()> {
    let args = cli::Cli::parse();
    match args.command {
        cli::Command::Run(run) => run_cmd(run),
    }
}

fn run_cmd(run: cli::RunArgs) -> Result<()> {
    // 1. Resolve the effective $HOME FIRST (R4.2), so every $HOME-relative grant in
    //    every layer is computed against the replacement home, not the real one.
    let layers = profile::resolved_layers(&run)?;
    let effective_home = home::resolve(&run, &layers)?;

    // 2. Load + merge profile layers (expands `~` against the effective home), then
    //    fold in --add-dirs / --home invocation overrides as the top layer.
    let profile = profile::load(&run, &effective_home)?;

    // 3. Build sanitized env (HOME authoritative, applied first).
    let env = env::build_minimal(&profile, &effective_home.path);

    // 4. Pick backend for this OS and apply policy.
    let backend = backends::select();

    if run.dry_run {
        backends::render_dry_run(&profile, &env, &run.cmd);
        return Ok(());
    }

    // Seed allowlisted real-home entries read-only into the (scratch) home (R4.4).
    home::seed(&effective_home)?;

    let code = backend.spawn(&profile, &env, &run.cmd)?;
    std::process::exit(code);
}
