#!/usr/bin/env just --justfile

export RUSTFLAGS := "-D warnings"
export RUSTDOCFLAGS := "-D warnings"

nightly := "nightly-2024-04-16"
msrv := "1.74"
rust := "stable"

# Run all checks
all: fmt check deny clippy examples docs test machete udeps msrv
    @echo "All checks passed 🍻"

# Check for unused dependencies
udeps:
    cargo +{{nightly}} hack udeps --each-feature

# Use machete to check for unused dependencies
machete:
    cargo +{{rust}} machete

# Check compilation across all features
check:
    cargo +{{rust}} hack check --no-private --each-feature --no-dev-deps
    cargo +{{rust}} hack check --no-private --feature-powerset --no-dev-deps --skip docs,axum
    cargo +{{rust}} check --all-targets --all-features

# Run clippy
clippy:
    cargo +{{rust}} clippy --all-targets --all-features -- -D warnings

# Check examples
examples:
    cargo +{{rust}} check --examples --all-features

# Build documentation
docs:
    cargo +{{rust}} doc --all-features --no-deps

# Check support for MSRV
msrv:
    cargo +{{msrv}} check --all-targets --all-features
    cargo +{{msrv}} doc --all-features --no-deps

# Run cargo tests
test:
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
