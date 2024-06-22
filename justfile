#!/usr/bin/env just --justfile

export RUSTFLAGS := "-D warnings"
export RUSTDOCFLAGS := "-D warnings"

nightly := "nightly-2024-04-16"
msrv := "1.74"
rust := env("RUSTUP_TOOLCHAIN", "stable")

# Run all checks
all: fmt check-all deny clippy examples docs test machete udeps msrv
    @echo "All checks passed 🍻"

# Check for unused dependencies
udeps:
    cargo +{{nightly}} hack udeps --each-feature

# Use machete to check for unused dependencies
machete:
    cargo +{{rust}} machete

alias c := check
# Check compilation
check:
    cargo +{{rust}} check --all-targets --all-features

# Check compilation across all features
check-all:
    cargo +{{rust}} check --all-targets --all-features
    cargo +{{rust}} hack check --no-private --each-feature --no-dev-deps
    cargo +{{rust}} hack check --no-private --feature-powerset --no-dev-deps --skip docs,axum,sni,pidfile

# Run clippy
clippy:
    cargo +{{rust}} clippy --all-targets --all-features -- -D warnings

# Check examples
examples:
    cargo +{{rust}} check --examples --all-features

# Build documentation
docs:
    cargo +{{rust}} doc --all-features --no-deps

# Build and read documentation
read: docs
    cargo +{{rust}} doc --all-features --no-deps --open

# Check support for MSRV
msrv:
    cargo +{{msrv}} check --all-targets --all-features
    cargo +{{msrv}} doc --all-features --no-deps


alias t := test
# Run cargo tests
test:
    cargo +{{rust}} test --all-features --no-run
    cargo +{{rust}} test --all-features

# Run coverage tests
coverage:
    cargo +{{rust}} tarpaulin -o html --all-features

# Run deny checks
deny:
    cargo +{{rust}} deny check

# Run fmt checks
fmt:
    cargo +{{rust}} fmt --all --check
