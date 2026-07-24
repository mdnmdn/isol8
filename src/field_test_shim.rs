//! Field-test binary is defined in isol8-cli; this target exists for feature parity.
//! Prefer: cargo run -p isol8-cli --features field-test --bin isol8-field-test
fn main() {
    eprintln!(
        "isol8-field-test: run via `cargo run -p isol8-cli --features field-test --bin isol8-field-test`"
    );
    std::process::exit(2);
}
