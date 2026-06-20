# isol8 — common dev commands. Run `just` to list.
# Requires: cargo (+ rustfmt, clippy components). `just ci` is the pre-commit gate.

set shell := ["bash", "-uc"]

# List available recipes.
default:
    @just --list

# Debug build.
build:
    cargo build

# Release build.
release:
    cargo build --release

# Run the binary (pass args: `just run --show-policies -- echo hi`).
run *args:
    cargo run -- {{args}}

# Unit + integration tests.
test:
    cargo test

# Field tests: real sandbox checks on an ad-hoc env/profile (see _docs/testing-strategies.md).
# Pass --keep to retain the temp workspace.
field-test *args:
    cargo run --bin isol8-field-test -- {{args}}

# Format sources.
fmt:
    cargo fmt --all

# Lint: format check + clippy with warnings denied (the CI lint gate).
lint:
    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings

# Type-check without building artifacts.
check:
    cargo check --all-targets

# Full pre-commit gate: everything CI runs.
ci: fmt-check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo build
    cargo test

# Format check only (used by `ci`).
fmt-check:
    cargo fmt --all -- --check

# Build API docs.
doc:
    cargo doc --no-deps

# Remove build artifacts.
clean:
    cargo clean
