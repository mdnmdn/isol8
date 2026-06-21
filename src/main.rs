use std::io::Write;

use anyhow::{bail, Context, Result};

use isol8::{backends, cli, config, profile, resolve};

fn main() -> Result<()> {
    match cli::parse() {
        cli::ParsedCli::Help => {
            cli::print_help();
            Ok(())
        }
        cli::ParsedCli::Version => {
            println!("isol8 {}", cli::version_string());
            Ok(())
        }
        cli::ParsedCli::Run(mut run) => {
            prepare_run(&mut run)?;
            run_cmd(run)
        }
        cli::ParsedCli::Init(init) => init_cmd(init),
        cli::ParsedCli::ProfilesList(list) => profiles_list_cmd(list),
        cli::ParsedCli::ProfilesShow(show) => profiles_show_cmd(show),
        cli::ParsedCli::Diag(diag) => diag_cmd(diag),
    }
}

fn diag_cmd(diag: cli::DiagArgs) -> Result<()> {
    let mut run = cli::RunInvocation {
        opts: diag.opts,
        cmd: diag.cmd,
    };
    prepare_run(&mut run)?;
    if run.cmd.is_empty() {
        bail!("@diag needs a command (e.g. isol8 @diag node --version)");
    }
    let args = cli::run_from(run.opts, run.cmd);
    isol8::diag::run(&args)
}

fn prepare_run(run: &mut cli::RunInvocation) -> Result<()> {
    let cli_auto = run.opts.auto_profiles_cli_override();
    let mut args = cli::run_from(run.opts.clone(), run.cmd.clone());
    let cfg = config::load()?;
    config::apply_to_run(&cfg, &mut args, cli_auto);
    config::apply_env_overrides(&mut args, cli_auto.is_some());
    if let Some(v) = cli_auto {
        args.opts.auto_profiles = v;
    }
    run.opts = args.opts;
    run.cmd = args.cmd;
    Ok(())
}

fn prepare_opts(opts: &mut cli::ProfileOpts) -> Result<()> {
    let cli_auto = opts.auto_profiles_cli_override();
    let cfg = config::load()?;
    let mut run = cli::run_from(opts.clone(), vec![]);
    config::apply_to_run(&cfg, &mut run, cli_auto);
    config::apply_env_overrides(&mut run, cli_auto.is_some());
    if let Some(v) = cli_auto {
        run.opts.auto_profiles = v;
    }
    *opts = run.opts;
    Ok(())
}

fn run_cmd(run: cli::RunInvocation) -> Result<()> {
    if run.show_policies() {
        if run.cmd.is_empty() {
            bail!("--show-policies requires a command (e.g. isol8 --show-policies -- echo hi)");
        }
        let args = cli::run_from(run.opts, run.cmd);
        let effective = resolve::effective_policy(&args)?;
        render_effective(&effective, &effective.cmd);
        return Ok(());
    }

    if run.show_profiles() {
        if run.cmd.is_empty() {
            return profiles_list(registry_from_run(&run)?, run.verbose());
        }
        let args = cli::run_from(run.opts, run.cmd);
        let effective = resolve::effective_policy(&args)?;
        println!("== selected layers ==");
        for (name, origin) in &effective.layer_names {
            println!("  {name} ({})", origin.label());
        }
        return Ok(());
    }

    if run.cmd.is_empty() {
        cli::print_help();
        return Ok(());
    }

    if std::env::var_os(isol8::env::SANDBOX_MARKER).is_some() {
        bail!(
            "isol8 is already running inside an isol8 sandbox — nested sandboxing is not \
             supported (macOS Seatbelt cannot nest). Run the command directly instead of \
             wrapping it in isol8 again."
        );
    }

    let args = cli::run_from(run.opts, run.cmd);
    let mut effective = resolve::effective_policy(&args)?;

    isol8::home::seed(&effective.home)?;
    resolve::confine_executable(&mut effective.profile, &mut effective.cmd)?;

    let backend = backends::select();
    let code = backend.spawn(&effective.profile, &effective.env, &effective.cmd)?;
    std::process::exit(code);
}

fn registry_from_run(run: &cli::RunInvocation) -> Result<profile::LayerRegistry> {
    profile::LayerRegistry::load(run.profile_paths())
}

fn render_effective(effective: &resolve::EffectivePolicy, cmd: &[String]) {
    println!("== layer stack ==");
    for (name, origin) in &effective.layer_names {
        println!("  {name} ({})", origin.label());
    }
    backends::render_dry_run(&effective.profile, &effective.env, cmd);
}

fn init_cmd(init: cli::InitArgs) -> Result<()> {
    let format = match init.format {
        cli::ConfigFormat::Toml => "toml",
        cli::ConfigFormat::Yaml => "yaml",
    };
    let path = init
        .path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config::default_init_path(format));
    if path.exists() {
        anyhow::bail!(
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

fn profiles_list_cmd(list: cli::ProfilesListArgs) -> Result<()> {
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

fn profiles_show_cmd(mut show: cli::ProfilesShowArgs) -> Result<()> {
    prepare_opts(&mut show.opts)?;
    let registry = profile::LayerRegistry::load(show.opts.profile_paths.as_slice())?;
    let Some(p) = registry.get(&show.name) else {
        anyhow::bail!("unknown profile '{}'", show.name);
    };
    let src = registry
        .source(&show.name)
        .map(|s| format!("{s:?}"))
        .unwrap_or_default();
    println!("# source: {src}");
    print!("{}", profile::format_layer(p)?);
    Ok(())
}
