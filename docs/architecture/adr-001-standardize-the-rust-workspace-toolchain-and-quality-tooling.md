---
author: owner
code_paths:
- Cargo.toml
- rust-toolchain.toml
- clippy.toml
- rustfmt.toml
- deny.toml
- lefthook.yml
date: 2026-06-17
status: accepted
tags:
- rust
- workspace
- toolchain
---

# Standardize the Rust workspace toolchain and quality tooling

Which Rust toolchain, workspace layout, and quality tools this multi-crate
project standardizes on.

## Context

A multi-crate Cargo workspace amplifies drift: each crate could pin a different
edition, MSRV, dependency version, or lint set, producing "works in crate A,
breaks in crate B" failures and spurious lint diffs. We want one pinned
toolchain and a single, inherited set of metadata, dependency versions, and
quality gates across every member crate.

## Decision

- Layout: member crates live under `crates/*`; `crates/core` holds library
  logic, `crates/app` is a thin binary that consumes it.
- Inheritance: edition, MSRV (`rust-version`), license, dependency versions, and
  lints are declared once in the root `Cargo.toml` (`[workspace.package]`,
  `[workspace.dependencies]`, `[workspace.lints]`) and pulled into each crate
  with `*.workspace = true`.
- Toolchain: pinned in `rust-toolchain.toml` so every crate builds identically.
- Format: `rustfmt`; Lint: `clippy` with `-D warnings`; Tests: `cargo nextest`
  (`--workspace`); Supply chain: `cargo deny`. All run via `lefthook` locally and
  mirrored in CI.

## Consequences


### Positive

- One pinned toolchain and one lint/metadata source for the whole workspace.
- Adding a crate under `crates/*` inherits all conventions automatically.

### Negative

- Bumping the toolchain or MSRV is a deliberate, reviewed change affecting every
  crate at once.
- `-D warnings` can block commits on new clippy lints after a toolchain bump.
