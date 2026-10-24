# Contributing

Thanks for contributing. This project ships a Nix dev shell and automated
quality gates so local checks match CI. The rules that matter most:

- every user-visible statement in this book and the README is grounded in
  the code or verified against the built binary — a doc claim that invents a
  flag, key or behaviour is a bug;
- the gate must be able to fail: a change to the Stop path that cannot make
  `tests/stop_gate_blocks.sh` print `gate: blocked as expected` is not done;
- suppression, fingerprints and the exit codes are a published contract —
  changing them is a spec (SPEC-006) amendment, not a patch.

## Development setup

1. Install [Nix](https://nixos.org/download) with flakes enabled.
2. Enter the dev shell: `nix develop` (or `direnv allow` to load `.envrc`).

The shell provides the pinned toolchain (`rust-toolchain.toml`),
`cargo-nextest`, `cargo-deny`, `cargo-mutants`, `cargo-rdme`, `typos`,
`lefthook` and `act`, and runs `lefthook install`, which wires the
pre-commit and commit-msg git hooks. `.githooks/commit-msg` enforces
Conventional Commits.

## Running checks locally

These mirror CI exactly:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo nextest run
cargo test --doc
cargo rdme --check
typos
```

The pre-commit hook runs `fmt`, `clippy` and `cargo rdme --check` on staged
Rust/README changes automatically.

The documentation has its own two gates:

```bash
nix shell nixpkgs#mdbook -c mdbook build mdbook        # the book must build
nix shell nixpkgs#markdownlint-cli2 -c markdownlint-cli2 "mdbook/guide/**/*.md" "README.md" "CONTRIBUTING.md"
```

markdownlint is configured by `.markdownlint-cli2.jsonc` (line length and a
few style rules are off; `docs/` and `.dg/` are excluded). Spelling is the
`typos` job, not markdownlint.

## Workspace layout

Three crates, one workspace (shared metadata, dependency versions and lints
in the root `Cargo.toml`, inherited with `*.workspace = true`):

| Crate | Contents |
|---|---|
| `crates/agent-slop-core` | the engine: comment extraction over tree-sitter grammars (`src/lang.rs`), the rule engine (`src/rules/`: lexicon, paraphrase, conditions, suppressions), the finding model and fingerprints (`src/finding.rs`), the three-tier config model (`src/config.rs`), and the rule corpus (`rules/*.toml`) |
| `crates/agent-slop-integrations` | everything that touches the outside: the six linter adapters (`src/linters/`), the Laya judge transport (`src/judge.rs`), telemetry (`src/telemetry.rs`) |
| `crates/agent-slop` | the binary: CLI parsing (`src/cli.rs`), scan/check rendering (`src/scan.rs`), hook dispatch (`src/hook.rs`), the ratchet (`src/ratchet/`) |

Dependency direction: `agent-slop` depends on both; the integrations crate
depends on the core; the core depends on neither. The ast-grep API is pinned
exactly (`=0.45.3`); bumps are deliberate.

`unsafe_code` is forbidden workspace-wide; `clippy::pedantic` and
`unwrap_used` are warnings that CI turns into errors (`-D warnings`). No
`unwrap()`/`expect()` in library code — `?` and proper error types
(`thiserror` for libs, `anyhow` for binaries). The binary carries no
argument-parsing crate and no regex crate; both are deliberate, and each
pattern interpreter (rule conditions, the referent grammar, the tsc line
format) has its own test module.

## Testing

The test runner is `cargo nextest` (profiles in `.config/nextest.toml`; the
CI profile retries twice). The layers:

- **Unit tests** live beside the code. The engine's tests pin the measured
  traps (one extraction per Rust doc comment, NFC folding of Russian, YAML
  key resolution), and the corpus tests assert every embedded TOML parses
  and every `error` rule declares an exception and an actionable note.
- **Rule fixtures** — `crates/agent-slop-core/tests/rules/fixtures/*.toml`.
  Each case is `{family, expect: hit|miss, lang, provenance, source}`.
  POSITIVE cases are harvested from real code and must carry `path:line`
  provenance — for `error` rules a positive without provenance fails the
  suite (POL-026 rule 3: the oracle must not be derived from the matcher
  being validated). Every rule must have at least one positive and one
  negative fixture. Add cases here when you touch a matcher.
- **Scenario tests** — `crates/agent-slop/src/ratchet/scenario_tests.rs`
  drive real scratch git repositories (commit mid-session, copied
  violations, moved violations, untracked debt, observe mode) against the
  ratchet.
- **End-to-end** — `crates/agent-slop/tests/` drives the *built binary*:
  `integration.rs` (the Stop/suppression matrix through `hook`),
  `self_test.rs` (the resolved-config checks), `telemetry.rs` (rows written
  by the real process), `judge_leg.rs` (a stub endpoint; asserts the
  annotation arrives, is additive, and that the confidence floor is
  load-bearing). Each test builds its own scratch git repo and points
  `HOME`/`XDG_CACHE_HOME` into it, so nothing reads your git config or
  writes a real telemetry root.
- **The gate proof** — `crates/agent-slop/tests/stop_gate_blocks.sh` runs
  the installed binary through five named Stop-gate scenarios; the Nix
  build fails unless every line appears. It is also shipped at
  `$out/share/agent-slop/stop_gate_blocks.sh`.
- **Nix checks** — `nix flake check` runs the package build (with tests),
  `clippy -D warnings`, `cargo fmt --check`, and an evaluated assertion of
  the home-manager module's rendered files (the `hm-module` check).

One consumer check is `#[ignore]`d and needs an external path:
`AGENT_SLOP_FEEDBACK_RECORD=<path to the consumer's feedback-record.sh>
cargo nextest run --ignored` verifies the SessionEnd rewrite keeps slop rows.

## Adding a rule

1. Write the TOML: `crates/agent-slop-core/rules/<family>.toml`, following
   `crates/agent-slop-core/rules/README.md`. Phrases and markers are the
   lexical half; anything structural (which AST node kinds count as
   comments) belongs in `src/lang.rs` and must agree with the compiled
   grammars.
2. Decide the level honestly: `error` requires a measured ~zero
   false-positive rate *and* a filled `[exception] shape`; every rule needs
   an actionable `[note]` (the corpus tests enforce both). A rule that
   cannot state its exception shape ships at `warning`.
3. Add the matcher arm in `crates/agent-slop-core/src/rules/mod.rs`
   (`evaluate_rule`): most rules are one `lex::…` call producing the
   message.
4. Add the family to `KNOWN_FAMILIES` in `src/config.rs` and give it its
   built-in level in `builtin_config()`. If the level is `error`, the
   frozen-blocking-set test fails until SPEC-006 is amended — that is the
   point of the test.
5. Fixtures: positives with provenance, negatives, hit and miss, in
   `crates/agent-slop-core/tests/rules/fixtures/`.
6. Update this book: a page under `mdbook/guide/rules/`, the rules table in
   [Configuration](./configuration.md), and — if the level is `error` — the
   blocking-set wording in
   [Hooks and the ratchet](./hooks-and-the-ratchet.md).

## Adding a linter adapter

One `mod` line plus one entry in the `ADAPTERS` table
(`crates/agent-slop-integrations/src/linters/mod.rs`): binary, argv, cost
class, `evaluates_repo_code`, path arity, extension list, parse function.
Record a fixture from the real binary under
`crates/agent-slop-integrations/tests/linters/fixtures/` and test the parser
against it — never spawn the tool in a test. Read the
[linter adapters](./linter-adapters.md) page first; it documents the traps
(extension filtering, one-target-per-run, select-after-parse).

## Running CI locally

The workflows run locally via [`act`](https://github.com/nektos/act)
(included in the dev shell). Requires Docker running and `gh auth login`;
pull the runner image once with `docker pull catthehacker/ubuntu:act-latest`.

```bash
./scripts/ci-local.sh             # all act-compatible jobs
./scripts/ci-local.sh --job fmt   # one job: fmt | clippy | test | msrv | deny | typos
```

The `flake-check` job is skipped under `act` (its installer needs systemd) —
run `nix flake check` in the dev shell instead. Your GitHub token is read
from `gh` per run and never written to disk.

## Commit convention

Commits follow [Conventional Commits](https://www.conventionalcommits.org/):

```text
<type>[(scope)][!]: <description>
```

Use a lowercase, imperative description (`add retry to client`). Append `!`
after the type/scope to mark a breaking change (`feat!: drop v1 API`).

| Type | Use for |
|---|---|
| feat | a new feature |
| fix | a bug fix |
| refactor | a change that neither fixes a bug nor adds a feature |
| perf | a performance improvement |
| test | adding or correcting tests |
| docs | documentation only |
| chore | maintenance or tooling |
| ci | CI configuration |
| build | build system or dependencies |
| revert | reverting a previous commit |

Enforced by `.githooks/commit-msg` locally and the `commit-lint.yml`
workflow in CI. The `prepare-commit-msg` hook strips AI attribution
trailers; the commit-msg hook also runs `dg hooks commit-msg`, and
`dg validate` keeps the decision graph under `docs/` consistent.

## Branch naming and pull requests

Branch from `main` with a type prefix: `feat/...`, `fix/...`, `chore/...`.
Keep a PR focused on a single change; run the checks above; fill in the PR
template; ensure CI is green. Merge strategy is left to the maintainers.

## Decisions

Non-trivial decisions are recorded with `dg` (DecisionGraph) under `docs/` —
see [AGENTS.md](../AGENTS.md). Propose the decision in your PR; a maintainer
confirms it before it is recorded.

## Conduct and security

By participating you agree to the
[Code of Conduct](../CODE_OF_CONDUCT.md). Report vulnerabilities privately
per [SECURITY.md](../SECURITY.md) — do not open public issues for security
reports.
