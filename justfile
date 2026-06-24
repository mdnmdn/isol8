# isol8 — common dev commands. Run `just` to list.
# Requires: cargo (+ rustfmt, clippy components). `just ci` is the pre-commit gate.

set shell := ["bash", "-uc"]

# List available recipes.
default:
    @just --list

# Debug build.
build:
    cargo build

# Build the Windows hook DLL and copy it beside isol8.exe (hybrid path enforcement).
build-winhook:
    cargo build -p isol8-winhook
    cp target/debug/isol8_winhook.dll target/debug/isol8-winhook.dll 2>/dev/null || cp target/debug/isol8-winhook.dll target/debug/

# Windows field-test helpers (hook DLL + file probe binary).
build-windows-test-deps: build-winhook
    cargo build --bin isol8-probe

# Release build.
release:
    cargo build --release

# Release build on Windows (isol8.exe + isol8-winhook.dll in target/release/).
release-windows:
    cargo build --release
    cargo build --release -p isol8-winhook
    cp target/release/isol8_winhook.dll target/release/isol8-winhook.dll

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

# Windows field tests with hook DLL deployed.
field-test-windows *args: build-windows-test-deps
    cargo run --bin isol8-field-test -- {{args}}


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
    cargo clippy --workspace --exclude isol8-winhook --all-features -- -D warnings

# Type-check without building artifacts.
check:
    cargo check --all-targets

# Full pre-commit gate: everything CI runs.
ci: fmt-check
    cargo clippy --workspace --exclude isol8-winhook --all-features -- -D warnings
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

# Bump release version: validate tag, lint+test, update Cargo.toml, commit and tag.
# Usage: `just bump 0.3.0`
bump version:
    bash _devops/scripts/version.sh bump {{version}}
