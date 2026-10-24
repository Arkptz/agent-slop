# Linter adapters

Besides its own rules, `scan` can fold external linters' findings into the
same finding model — same shape, same severity mapping, same fingerprint
identity, same telemetry. `check` runs no adapters. Adapters parse a
fixture-recorded output format; exit status is never an error signal (statix
exits 1 on findings, go vet exits 0 with findings), only stdout is read.

## The catalogue

| Tool | Rule ids | Cost | Languages | Targets |
|---|---|---|---|---|
| `ruff` | `ruff/<code>` | A | `.py`, `.pyi` | all named paths in one run |
| `deadnix` | `deadnix/dead-code` | A | `.nix` | all named paths in one run |
| `statix` | `statix/W04`, … | A | `.nix` | **one run per path** |
| `clippy` | `clippy/<lint>` | D | `.rs` | whole project, ignores paths |
| `govet` | `govet/<analyzer>` | C | `.go` | whole project (`./...`) |
| `tsc` | `tsc/<code>` | C | `.ts`, `.tsx`, `.mts`, `.cts` | whole project |

Exact invocations (with the repo root as working directory, so each tool's
own config discovery fires):

- `ruff check --output-format json --force-exclude`
- `deadnix --output-format json`
- `statix check -o json`
- `cargo clippy --message-format json --all-targets`
- `go vet -json ./...`
- `tsc --noEmit --pretty false`

## Cost classes

SPEC-006 classes, and what `--linters auto` selects:

- **A** — under 200 ms, no build: `ruff`, `deadnix`, `statix`.
- **B** — about 1 s process start. No shipped adapter; the class exists
  because eligibility compares against it and a gap in the scale would make
  the comparison lie.
- **C** — seconds, needs a warm build cache: `govet`, `tsc`.
- **D** — minutes, exclusive lock: `clippy`.

`--linters auto` (the default) runs **A and B only**. `--linters none` runs
no adapters. `--linters ruff,clippy` runs exactly the named adapters,
waiving cost classes — that is what naming means.

## Eligibility

An adapter runs only when all of these hold:

1. `[slop.linters.<name>]` is absent, or present with `enable = true`;
2. the selection (`auto`/`none`/named) admits it;
3. it is not an adapter that **evaluates repository code**, unless it was
   named explicitly *and* the stage is a scan. `clippy` executes `build.rs`
   and proc macros; `tsc` executes `tsconfig.json` extends chains and
   plugins from `node_modules`. Such an adapter is never selected by `auto`
   and never runs in a hook, because a hook fires on a clone the user never
   inspected.

## Extension filtering

A target is handed to an adapter only if its extension is in the adapter's
set. This is load-bearing: `ruff check` given a `.nix` or `.rs` file parses
it as Python and reports `invalid-syntax` per line, which on one repository
produced ~198k fabricated findings — 98.9% of the report — and buried the
real ones.

A path-taking linter handed *no* matching paths skips its exec entirely;
linting the working directory instead of nothing is the bug that rule
prevents.

## Severities and selection

- An imported finding's level comes from configuration, in order: the id's
  entry in `[slop.rules.overrides]`, then the linter's `severity`
  (default `warning`). A linter's own severity can *never* produce
  `error` — the only path to `error` is an explicit `overrides`
  promotion. The adapter's own read of a code (stylistic → `hint`) can
  only *weaken* what config says, never strengthen it.
- Stylistic codes become `hint`: for ruff, codes beginning with `E`, `W`,
  `D`, `Q` (the families ruff documents as formatting-only).
- `select`/`ignore` are applied *after* parsing, by code prefix — never
  passed on the command line, where ruff's `--select` would *replace* the
  project's own selection rather than add to it. Unset means the linter's
  own config decides.
- Foreign ids are never rewritten (`clippy::needless_return` is stripped to
  `clippy/needless_return`; statix's integer `4` is zero-padded into
  `statix/W04`, matching `statix list`), so telemetry joins with upstream
  documentation.
- A finding's snippet is its message (capped at 512 characters), not the
  source line: the ratchet only needs a stable identity per finding, and a
  message survives reindentation where a line does not.

## Skip reasons

A tool that did not run is carried in the JSON envelope's `skipped[]` array
and in human output (`skipped <tool>: <reason>`), never an error:

| Reason | Meaning |
|---|---|
| `absent` | the binary is not on `PATH` |
| `no-targets` | no named path matches the adapter's extensions |
| `spawn failed: …` | the process could not be started |

Human output also lists skipped *paths* (`excluded`, `unreadable`,
`binary`) and the telemetry row records a per-tool status map
(`{"ruff": "ok"}`, `"statix": "skipped:absent"`).

## Adding an adapter

One `mod` line plus one entry in the `ADAPTERS` table of
`crates/agent-slop-integrations/src/linters/mod.rs`: tool name, binary,
argv, cost class, `evaluates_repo_code`, path arity, extension list, and a
parse function. Parse functions are pure and tested against fixtures
recorded from the real binary under
`crates/agent-slop-integrations/tests/linters/fixtures/`.
