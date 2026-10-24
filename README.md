# Rust Workspace Starter

A multi-crate Rust Cargo workspace: reproducible tooling, hardened CI, and built-in decision tracking.

[![CI](https://github.com/Arkptz/agent-slop/actions/workflows/ci.yml/badge.svg)](https://github.com/Arkptz/agent-slop/actions/workflows/ci.yml)
[![docs.rs](https://img.shields.io/docsrs/agent_slop)](https://docs.rs/agent_slop)
[![crates.io](https://img.shields.io/crates/v/agent_slop.svg)](https://crates.io/crates/agent_slop)
[![MSRV](https://img.shields.io/crates/msrv/agent_slop.svg)](Cargo.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

## Overview

<!-- cargo-rdme start -->

Comment extraction, rule evaluation, suppressions and configuration
for agent-slop.

<!-- cargo-rdme end -->

## Features

- **Cargo workspace** — member crates live under `crates/*` and share one version, MSRV, lint set, and dependency table from the root `Cargo.toml`.
- **Reproducible toolchain** — a Nix dev shell pins Rust via `rust-toolchain.toml`, plus `cargo-nextest`, `cargo-deny`, `cargo-mutants`, and `cargo-rdme`.
- **Hardened CI** — every GitHub Action is pinned to a commit SHA; jobs cover `fmt`, `clippy -D warnings`, tests, the declared MSRV, and a supply-chain audit.
- **README stays in sync** — `cargo rdme` regenerates the overview above from the `app-core` crate's `//!` documentation, enforced in CI.
- **Decision graph** — non-trivial decisions are tracked with `dg` under `docs/`.

## Getting started

```sh
nix develop                     # enter the dev shell (or `direnv allow`)
cargo nextest run --workspace   # run every crate's tests
cargo run -p app -- Ada         # run the binary crate
```

## Development

The dev shell mirrors CI. The commands you will use most:

```sh
cargo nextest run --workspace                            # run all tests
cargo clippy --workspace --all-targets -- -D warnings    # lint (zero warnings)
cargo fmt --all                                          # format
cargo deny check                                         # supply-chain audit
cargo rdme --check                                       # verify the README is in sync
./scripts/ci-local.sh                                    # run CI jobs locally via act
```

## Project layout

```text
.
├── crates/
│   ├── core/       # app-core library; the overview above is generated from its //! docs
│   │   └── src/lib.rs
│   └── app/        # binary crate that depends on app-core
│       └── src/main.rs
├── Cargo.toml      # workspace: members, shared metadata, lints, and MSRV
├── flake.nix       # Nix dev shell + flake checks
├── compose.yaml    # local Postgres + Redis stack
└── docs/           # dg decision records
```

Add a crate with `cargo new --lib crates/<name>`; the `crates/*` glob picks it up automatically.

## Running CI locally

`./scripts/ci-local.sh` runs the act-compatible jobs; the `flake-check` job is skipped under act, so run `nix flake check` in the dev shell instead. See [CONTRIBUTING.md](CONTRIBUTING.md) for prerequisites.

## Decisions

This project tracks decisions, policies, and incidents with `dg` under `docs/`. See [AGENTS.md](AGENTS.md) for the workflow.

## License

Dual-licensed under either MIT or Apache-2.0 at your option (`SPDX-License-Identifier: MIT OR Apache-2.0`). Add the matching `LICENSE-MIT` and `LICENSE-APACHE` files to your project.
