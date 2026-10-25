# Project conventions (Rust)

## Stack

- Language: Rust (pinned via `rust-toolchain.toml`)
- Test runner: `cargo nextest` (preferred over `cargo test`)
- Formatter: `rustfmt` (enforced by pre-commit + agent hooks)
- Linter: `clippy` with `-D warnings` (zero-warning policy)

## Commands

```bash
cargo build --workspace              # build all crates
cargo nextest run --workspace        # run tests across the workspace
cargo clippy --all-targets --workspace -- -D warnings   # lint (must pass clean)
cargo fmt --all --check              # format check
cargo deny check                     # licenses + advisories + bans
```

## Workspace layout

- Cargo workspace: member crates live under `crates/*`.
- `crates/agent-slop-core` — comment extraction, rule engine, config model and
  the rule corpus; `crates/agent-slop-integrations` — linter adapters, judge
  transport, telemetry; `crates/agent-slop-protocol` — the serde wire types of
  the plugin protocol; `crates/agent-slop` — the binary (CLI, hook, ratchet).
  Every crate is published to crates.io; check with
  `cargo publish --workspace --dry-run --allow-dirty`.
- Shared metadata, dependency versions, and lints are defined once in the root
  `Cargo.toml` (`[workspace.package]`, `[workspace.dependencies]`,
  `[workspace.lints]`) and inherited per crate with `*.workspace = true`.

## Critical rules

- ALWAYS run `cargo fmt` before committing — pre-commit + the agent hooks enforce it.
- NEVER commit with clippy warnings — `-D warnings` is enforced.
- NEVER `unwrap()`/`expect()` in library code — use `?` and proper error types
  (`thiserror` for libs, `anyhow` for binaries).
- NEVER hand-edit `Cargo.lock` or `rust-toolchain.toml` without discussion.
- Prefer `cargo nextest run` over `cargo test`.

## Agent setup in this project

- `CLAUDE.md` re-exports `AGENTS.md` (`@AGENTS.md`) so Claude Code auto-loads these conventions. Per-agent permissions/MCP are the developer's own setup, not shipped with this template.
- `dg` (DecisionGraph) governs decisions/docs — see below.

---

<!-- BEGIN dg-section (generated from _shared/AGENTS.dg-section.md; do not edit) -->

# Decision Graph

This project uses `dg` to track decisions, policies, specs, and operational
knowledge as a markdown knowledge graph under `docs/`.

**Command reference is in the CLI, not here.** Run `dg guide` for the workflow,
`dg --help` for all subcommands, and `dg new --help` / `dg set --help` for flags.
Do not rely on a copy of the help text — query the tool.

## Rules an agent must follow

- **Ask first, create later.** For vague requests, ask 3-5 clarifying questions
  (via `AskUserQuestion` / `ask_user`, never a wall of plain text) and wait for
  answers. Goal: complete documents with no TBD/FIXME. Only use TBD/FIXME if the
  user says they don't know or tells you to proceed without it.
- **Human-gated vs agent-creatable.** ADR / POL / OPP / INC / SPEC / PROC are
  human-gated: propose, get confirmation, then create. `TASK` docs you may create
  autonomously.
- **Always mutate through `dg`** (`dg new` / `dg set`) — never hand-edit files
  under `docs/` or `.dg/`. `dg` assigns IDs, validates schema, and keeps the
  graph consistent. For multi-line / table / special-character content, get the
  path (`dg show ID --json`), edit the file, then `dg validate`.
- **Field assignment footgun:** `key=value` sets a scalar; `key+=value` appends
  to an array. Only `supersedes` / `superseded_by` are scalar. `enables`,
  `enabled_by`, `triggers`, `triggered_by`, `implements`, `depends_on`,
  `related`, `conflicts_with`, `tags`, `code_paths` are arrays — use `+=`.
  (`dg set ADR-001 superseded_by=ADR-010` ✓ scalar; `dg set OPP-001 enabled_by+=ADR-010` ✓ array.)
- **Quality:** problem-focused titles; cross-link with the most specific relation;
  add yourself to authors.

## End of session

Run `dg validate` (also a pre-commit hook) and `dg lint` to catch orphans and
dangling refs before finishing. `dg hooks stop` summarizes outstanding issues
(run it manually unless your agent has a native Stop hook).

<!-- END dg-section -->
