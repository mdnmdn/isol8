//! `isol8` binary — installs the offline-registry provider, then runs the CLI.

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "registry")]
    isol8::ensure_registry_provider();
    isol8_cli::cli::main()
}
