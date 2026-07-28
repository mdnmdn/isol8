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

# Unit + integration tests (full workspace).
test:
    cargo test --workspace

# Field tests: real sandbox checks on an ad-hoc env/profile (see _docs/testing-strategies.md).
# Pass --keep to retain the temp workspace.
field-test *args:
    cargo run --features field-test --bin isol8-field-test -- {{args}}


local-publish:
    cargo build
    cp ./target/debug/isol8 ~/.local/bin/isol8
    echo published to ~/.local/bin/isol8

# Format sources.
fmt:
    cargo fmt --all

# Lint: format check + clippy with warnings denied (the CI lint gate).
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    # Public API docs are the embedding contract — broken links fail the lint.
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

# Type-check without building artifacts.
check:
    cargo check --workspace --all-targets
    cargo check -p isol8 --no-default-features
    cargo check -p isol8 --no-default-features --features registry

# Full pre-commit gate: everything CI runs.
ci: fmt-check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    # The advertised embedding tiers must actually compile (_docs/embedding.md §1).
    cargo check -p isol8 --no-default-features
    cargo check -p isol8 --no-default-features --features registry
    cargo check -p isol8 --no-default-features --features wizard
    cargo build --workspace
    cargo build --workspace --examples
    cargo test --workspace

# Format check only (used by `ci`).
fmt-check:
    cargo fmt --all -- --check

# Build API docs.
doc:
    cargo doc --no-deps

# Remove build artifacts.
clean:
    cargo clean

# Bump release version: validate tag, lint+test, update Cargo.toml, commit and tag.
# Usage: `just bump 0.3.0`
bump version:
    bash _devops/scripts/version.sh bump {{version}}
