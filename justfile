# isol8 — common dev commands. Run `just` to list.
# Requires: cargo (+ rustfmt, clippy components). `just ci` is the pre-commit gate.

set shell := ["bash", "-uc"]

# List available recipes.
default:
    @just --list

# Debug build.
build:
    cargo build

# Build the Windows hook DLL (also built automatically by isol8-core's build.rs on Windows).
build-winhook:
    cargo build -p isol8-winhook

# Windows field-test helpers (probe binary; hook DLL is embedded in isol8-core).
build-windows-test-deps:
    cargo build -p isol8-cli --bin isol8-probe
    cargo build -p isol8-cli --features field-test --bin isol8-field-test

# Release build.
release:
    cargo build --release

# Release build on Windows (hook DLL embedded).
release-windows:
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
    cargo run -p isol8-cli --features field-test --bin isol8-field-test -- {{args}}

# Windows field tests with probe binary (hook DLL is embedded).
field-test-windows *args: build-windows-test-deps
    cargo run -p isol8-cli --features field-test --bin isol8-field-test -- {{args}}


# Dry-run the crates.io publish (ordering + packaging; uploads nothing).
publish-dry:
    ./_devops/scripts/publish.sh --dry-run

# Publish the whole workspace to crates.io, leaves first. Needs CARGO_REGISTRY_TOKEN.
publish:
    ./_devops/scripts/publish.sh

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
    cargo clippy --workspace --exclude isol8-winhook --all-targets --all-features -- -D warnings
    # Public API docs are the embedding contract — broken links fail the lint.
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

# Type-check without building artifacts.
check:
    cargo check --workspace --all-targets
    cargo check -p isol8 --no-default-features
    cargo check -p isol8 --no-default-features --features registry

# Full pre-commit gate: everything CI runs.
ci: fmt-check
    cargo clippy --workspace --exclude isol8-winhook --all-targets --all-features -- -D warnings
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
