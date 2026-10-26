<div align="center">

# agent-slop

**A deterministic linter for the comments AI coding agents leave behind, with a ratcheting Stop gate for agent sessions.**

[![CI](https://github.com/Arkptz/agent-slop/actions/workflows/ci.yml/badge.svg)](https://github.com/Arkptz/agent-slop/actions/workflows/ci.yml)
[![Docs](https://github.com/Arkptz/agent-slop/actions/workflows/docs.yml/badge.svg)](https://github.com/Arkptz/agent-slop/actions/workflows/docs.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-orange.svg)](Cargo.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

</div>

```text
$ agent-slop check demo.py
demo.py:1:1 slop/comment-history — change-history residue 'previously' — narrates the edit, not the code
demo.py:4:1 slop/todo-no-condition — TODO with no removal condition — name one or delete it
```

## Why

Coding agents write comments for the reviewer of the current session, not
for the next reader of the file: "we previously sent a Referer here",
"for now", "as requested", a docstring that repeats the function name, a
TODO nobody will ever be able to close. agent-slop finds that residue
with tree-sitter (through `ast-grep`) and a reviewed phrase corpus, so the
same input always gives the same findings and no model sits in the loop.
Run as a harness hook, it tells the agent about the problem in the
agent's own protocol and blocks the session only when the session itself
added an error.

## Quick start

```sh
nix run github:Arkptz/agent-slop -- scan                # whole repository
nix run github:Arkptz/agent-slop -- scan --since main   # only what this branch changed
```

## What it does

- **Sixteen rule families** for comment slop: change history, session
  meta, review jargon, comments that restate the code, TODOs without a
  removal condition, unsourced performance and constraint claims, hedging,
  step narration, and more. Each rule has a page with bad and good
  examples in the [rule reference](mdbook/guide/rules/index.md).
- **Twelve languages**: Rust, Java, Python, Go, TypeScript, TSX,
  JavaScript, Bash, Nix, C, C++ and YAML. Most phrase lists cover English
  and Russian.
- **A ratchet, not an audit.** The `SessionStart` hook pins a baseline,
  `Stop` compares against it and, in `block` mode, exits 2 only for
  errors the session added. Existing debt never blocks.
- **Fail-open everywhere.** No git, a broken config tier or an expired
  deadline degrades the scan; it never turns into a failed session.
- **Suppressions with a referent.** `agent-slop: allow <rule> -- <reason>`
  is accepted only when the reason names something checkable: a URL, a
  decision-doc id, an issue, a path or a date.
- **Extensible without forking**: custom TOML rules with fixtures that run
  through the real engine, tuning of the built-in phrase lists, and
  external plugins over a JSON-on-stdio protocol.

## How it compares

| | agent-slop | general linters (clippy, ruff, eslint) | an LLM reviewer |
|---|---|---|---|
| Looks at comment wording | yes | style and spelling only | yes |
| Same input, same result | yes | yes | no |
| Blocks only what a session added | yes (ratchet) | no | no |
| Runs inside the agent's hook loop | yes | through wrappers | rarely |
| Cost per run | local, milliseconds | local | tokens and latency |

agent-slop sits next to your existing linters: it imports ruff, clippy,
deadnix, statix, govet and tsc findings into the same report and ratchet.

## Installation

<details>
<summary><b>Nix flake / home-manager</b></summary>

```nix
# flake.nix inputs
agent-slop.url = "github:Arkptz/agent-slop";

# home-manager
imports = [ inputs.agent-slop.homeManagerModules.default ];
programs.agent-slop = {
  enable = true;
  hooks.grok.enable = true;
  hooks.claude.enable = true; # exposes programs.agent-slop.claudeHooks
};
```

The module installs the binary, writes the user config tier and the grok
hook file, and lints every custom rule you declare at build time. Full
option reference: [Installation](mdbook/guide/installation.md).

</details>

<details>
<summary><b>Cargo</b></summary>

```sh
git clone https://github.com/Arkptz/agent-slop
cd agent-slop
cargo build --release -p agent-slop   # binary at target/release/agent-slop
```

</details>

## Usage

```sh
agent-slop scan                                # human output on a tty, JSON otherwise
agent-slop scan --since main --fail-on error   # CI: fail if the branch added an error
agent-slop check src/main.rs                   # advisory, no git needed, always exits 0
agent-slop explain slop/comment-history        # what a rule means, and its suppression line
```

A legitimate exception is suppressed on the line it covers:

```python
# agent-slop: allow slop/comment-history -- rollback steps required by ADR-031
# We previously always sent a Referer header here.
referer = None
```

<details>
<summary><b>Harness integration</b></summary>

`agent-slop hook` reads one hook payload on stdin and dispatches on the
event name. Claude Code (`snake_case`) and grok (camelCase) payloads are
both understood.

| Event | Behaviour | Exit |
|---|---|---|
| `PostToolUse` | advice on the file just written, as `additionalContext` | always 0 |
| `SessionStart` | pins the ratchet baseline | always 0 |
| `Stop` | names every added error, its legitimate exception and the suppression line | 0 allow, 2 block |

Details: [Hooks and the ratchet](mdbook/guide/hooks-and-the-ratchet.md).

</details>

<details>
<summary><b>Configuration and extension</b></summary>

Configuration has three tiers: built-ins, the user file
`~/.config/agent-slop/config.toml`, and the repository's
`.agent-hooks.toml`. A repository sets its own rule levels and mode, like
any linter config; only the user tier may declare what executes.

- [Configuration](mdbook/guide/configuration.md): every `[slop]` key.
- [Custom rules](mdbook/guide/custom-rules.md): one TOML file per rule,
  with fixtures; promotion to `error` requires green strict lint.
- [Tuning built-ins](mdbook/guide/tuning.md): extend or prune phrase
  lists, adjust thresholds within audited ranges.
- [Writing a plugin](mdbook/guide/plugins.md): the `agent-slop-plugin/1`
  protocol (`describe`, then `check`).

</details>

## Documentation

The [user guide](mdbook/guide/introduction.md) covers the CLI, the three
configuration tiers, every rule, suppressions, the Stop gate, linter
adapters, custom rules, tuning, plugins, telemetry and the optional judge.
Build it with `mdbook build mdbook`.

## Library crates

<!-- cargo-rdme start -->

Comment extraction, rule evaluation, suppressions and configuration
for agent-slop.

This crate is the deterministic engine behind the `agent-slop` binary.
It parses source with tree-sitter grammars (through `ast-grep`), pulls
out every comment and Python docstring, and matches them against a
reviewed corpus of sixteen rule families embedded from
`rules/*.toml`. Nothing in it spawns a process, reads the network or
consults a model; the binary and the integrations crate own those.

### Modules

- `lang` — the language table (Rust, Java, Python, Go, TypeScript,
  TSX, JavaScript, Bash, Nix, C, C++, YAML) and the comment walk.
- `rules` — the scan engine: lexical matchers, the structural
  paraphrase check, the TODO-condition interpreter and the
  `agent-slop: allow` suppression parser.
- `finding` — the `Finding` model and the ratchet identity: a
  path-independent 16-hex-character fingerprint.
- `config` — the three-tier `[slop]` configuration (built-ins, the
  user file, the repository's `.agent-hooks.toml`), resolved without
  ever failing, plus the `[slop.tune]` adjustments and the
  `[slop.plugins]` declarations.

### Example

```rust
use agent_slop_core::{config, rules};

let cfg = config::builtin_config();
let source = "# We previously always sent a Referer header.\nreferer = None\n";
let findings = rules::scan_source(&cfg, "client.py", source);

assert_eq!(findings.len(), 1);
assert_eq!(findings[0].rule, "slop/comment-history");
```

<!-- cargo-rdme end -->

| Crate | Contents |
|---|---|
| `agent-slop-core` | comment extraction, rule engine, suppressions, configuration |
| `agent-slop-protocol` | wire types of the plugin protocol |
| `agent-slop-integrations` | linter adapters, plugin host, judge, telemetry |
| `agent-slop` | the binary: `scan`, `check`, `hook`, `rules`, `self-test` |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The dev shell mirrors CI:

```sh
nix develop
cargo nextest run --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo rdme --check
```

Design decisions are tracked with `dg` under `docs/`; see
[AGENTS.md](AGENTS.md).

## Security

Report vulnerabilities privately; see [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
