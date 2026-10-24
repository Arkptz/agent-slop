# agent-slop

A deterministic, AST-based linter for AI-agent comment slop, with a
ratcheting Stop gate for coding-agent sessions. agent-slop parses your
source with tree-sitter grammars (via `ast-grep`), extracts comments and
docstrings, and matches them against a reviewed corpus of phrase lists,
anchored markers and structural checks. It tells an AI agent — in its own
hook protocol — that "we previously sent a Referer" is not documentation.

[![CI](https://github.com/Arkptz/agent-slop/actions/workflows/ci.yml/badge.svg)](https://github.com/Arkptz/agent-slop/actions/workflows/ci.yml)
[![docs.rs](https://img.shields.io/docsrs/agent_slop)](https://docs.rs/agent_slop)
[![crates.io](https://img.shields.io/crates/v/agent_slop.svg)](https://crates.io/crates/agent_slop)
[![MSRV](https://img.shields.io/crates/msrv/agent_slop.svg)](Cargo.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

<!-- cargo-rdme start -->

Comment extraction, rule evaluation, suppressions and configuration
for agent-slop.

This crate is the deterministic engine behind the `agent-slop` binary.
It parses source with tree-sitter grammars (through `ast-grep`), pulls
out every comment and Python docstring, and matches them against a
reviewed corpus of sixteen rule families embedded from
`rules/*.toml`. Nothing in it spawns a process, reads the network or
consults a model; the binary and the integrations crate own those.

## Modules

- `lang` — the language table (Rust, Java, Python, Go, TypeScript,
  TSX, JavaScript, Bash, Nix, C, C++, YAML) and the comment walk.
- `rules` — the scan engine: lexical matchers, the structural
  paraphrase check, the TODO-condition interpreter and the
  `agent-slop: allow` suppression parser.
- `finding` — the `Finding` model and the ratchet identity: a
  path-independent 16-hex-character fingerprint.
- `config` — the three-tier `[slop]` configuration (built-ins, the
  user file, the repository's `.agent-hooks.toml`), resolved without
  ever failing.

## Example

```rust
use agent_slop_core::{config, rules};

let cfg = config::builtin_config();
let source = "# We previously always sent a Referer header.\nreferer = None\n";
let findings = rules::scan_source(&cfg, "client.py", source);

assert_eq!(findings.len(), 1);
assert_eq!(findings[0].rule, "slop/comment-history");
```

<!-- cargo-rdme end -->

## What the tool is

The binary is two things:

- **A linter for comment slop.** Sixteen rule families (`slop/comment-history`,
  `slop/todo-no-condition`, `slop/review-jargon`, …) catch the residue AI
  coding agents leave in source comments: change-history narration, session
  meta ("for now", "as requested"), review vocabulary ("nit:", "LGTM"),
  comments that restate the code, TODOs with no removal condition, unsourced
  perf claims and constraints. Matching is deterministic — no model in the
  loop — and bilingual by design: most phrase lists cover English and
  Russian, with stems for Russian inflection. Rules run over Rust, Java, Python, Go, TypeScript, TSX,
  JavaScript, Bash, Nix, C, C++ and YAML.
- **A ratcheting Stop gate.** Registered as a harness hook, it pins a
  baseline at `SessionStart`, recomputes the session's added findings at
  `Stop`, and — in `block` mode — exits 2 when the session *added* an error
  the baseline does not account for. Existing debt never blocks: the gate is
  a ratchet, not an audit.

Everything about it is fail-open: no git, no repository, a malformed config
tier, an expired deadline — the scan degrades, never the exit code. A gate
that can crash a session is worse than no gate.

## Install

With [Nix](https://nixos.org) and flakes:

```sh
# run it without installing
nix run github:Arkptz/agent-slop -- scan

# or add the package / home-manager module to your flake inputs
```

As a home-manager module (installs the binary, the user config tier and the
grok hook source; see [the book](mdbook/guide/installation.md) for the full
option reference):

```nix
programs.agent-slop = {
  enable = true;
  hooks.grok.enable = true;
  hooks.claude.enable = true; # exposes programs.agent-slop.claudeHooks
};
```

With cargo, from a clone of this repository:

```sh
cargo build --release -p agent-slop   # binary at target/release/agent-slop
```

## 60-second quickstart

```sh
# Scan the whole repository (human output on a tty, JSON otherwise)
agent-slop scan

# Scan only what differs from main, and fail if anything was added at error
agent-slop scan --since main --fail-on error

# Check named files, no git needed, always exit 0
agent-slop check src/main.rs

# What a rule means, and the ready-made suppression line
agent-slop explain slop/comment-history
```

Real output of `check` on a slopped file:

```text
demo.py:1:1 slop/comment-history — change-history residue 'previously' — narrates the edit, not the code
```

Suppress one finding on one line — the reason must carry a referent (URL,
decision-doc id, issue ref, path, or `date:YYYY-MM-DD`):

```python
# agent-slop: allow slop/comment-history -- rollback steps required by ADR-031
# We previously always sent a Referer header here.
referer = None
```

## Harness integration

`agent-slop hook` reads one hook payload on stdin and dispatches on the
event name; every leg is fail-open. The home-manager module wires grok
automatically and exposes the Claude registrations as a read-only value:

| Leg | Behaviour | Exit |
|---|---|---|
| `PostToolUse` | advice on the file just written, as `additionalContext` | always 0 |
| `SessionStart` | pins the ratchet baseline for the session | always 0 |
| `Stop` | ratchet against the pinned baseline; block message names every added error, its legitimate exception and the suppression line | 0 allow, 2 block |

Both Claude Code (`snake_case`) and grok (camelCase, and both spellings in
one payload) envelopes are understood; the payload difference is invisible
to configuration. Full protocol and payload details:
[Hooks and the ratchet](mdbook/guide/hooks-and-the-ratchet.md).

## Documentation

The [user guide](mdbook/guide/introduction.md) covers the CLI reference, the
three configuration tiers, every rule with bad/good examples, suppressions,
the Stop gate, the built-in linter adapters, telemetry, and the optional
Laya judge. Build it locally with `mdbook build mdbook`.

## Development

The dev shell mirrors CI. The commands you will use most:

```sh
cargo nextest run --workspace                            # run all tests
cargo clippy --workspace --all-targets -- -D warnings    # lint (zero warnings)
cargo fmt --all                                          # format
cargo rdme --check                                       # verify the README is in sync
./scripts/ci-local.sh                                    # run CI jobs locally via act
```

## Project layout

```text
.
├── crates/
│   ├── agent-slop-core/           # comment extraction, rule engine, config model
│   ├── agent-slop-integrations/   # linter adapters, judge, telemetry
│   └── agent-slop/                # the binary: scan, check, hook, ratchet
├── crates/agent-slop-core/rules/  # the rule corpus (one TOML per family)
├── mdbook/                        # the user guide (book.toml, guide/)
├── nix/                           # package.nix, hm-module.nix
└── flake.nix                      # packages, checks, dev shell, HM module
```

## Decisions

This project tracks decisions, policies, and incidents with `dg` under
`docs/`. See [AGENTS.md](AGENTS.md) for the workflow.

## License

Dual-licensed under either MIT or Apache-2.0 at your option
(`SPDX-License-Identifier: MIT OR Apache-2.0`).
