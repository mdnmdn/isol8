//! The `isol8` binary is a thin shim over the `cli` feature's entry point; all the
//! argument parsing and command glue lives in `isol8::cli`.

fn main() -> anyhow::Result<()> {
    isol8::cli::main()
}
