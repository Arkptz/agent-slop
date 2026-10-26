# Contributing

Thanks for contributing. This project ships a Nix dev shell and automated
quality gates so local checks match CI. The full contributor guide —
workspace layout, the test layers, adding a rule or a linter adapter — lives
in [the book](mdbook/guide/contributing.md); this file is the short version.

## Development setup

1. Install [Nix](https://nixos.org/download) with flakes enabled.
2. Enter the dev shell: `nix develop` (or `direnv allow` to load `.envrc`).

The shell provides the pinned toolchain (`rust-toolchain.toml`),
`cargo-nextest`, `cargo-deny`, `cargo-mutants`, `cargo-rdme`, `typos`,
`lefthook` and `act`, and runs `lefthook install`, which wires the
pre-commit and commit-msg git hooks. `.githooks/commit-msg` enforces
Conventional Commits (zero-dependency POSIX `sh`).

## Running checks locally

These mirror CI exactly:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings  # pedantic + panic-free (expect/panic allowed in tests)
cargo nextest run
cargo test --doc
cargo rdme --check          # the README stays in sync with the crate docs
typos
nix shell nixpkgs#mdbook -c mdbook build mdbook   # the user guide builds
```

The pre-commit hook runs `fmt`, `clippy` and `cargo rdme --check` on staged
changes automatically.

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
|----------|---------|
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

## Branch naming

Branch from `main` with a type prefix: `feat/...`, `fix/...`, `chore/...`.

## Pull requests

1. Branch from `main`.
2. Keep the PR focused on a single change.
3. Run the checks above and confirm they pass.
4. Fill in the PR template.
5. Ensure CI is green.

Merge strategy is left to the maintainers.

## Decisions

Non-trivial decisions are recorded with `dg` (DecisionGraph) under `docs/` —
see [AGENTS.md](AGENTS.md). Propose the decision in your PR; a maintainer
confirms it before it is recorded.

## Conduct and security

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
Report vulnerabilities privately per [SECURITY.md](SECURITY.md) — do not
open public issues for security reports.
