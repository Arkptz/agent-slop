# CLI reference

One binary, seven subcommands. The surface is hand-parsed (no
argument-parsing crate); the exit codes are a published contract:

| Exit | Meaning |
|---|---|
| `0` | success: nothing at or above `--fail-on`, an allowed verdict, or an informational command |
| `1` | `scan`: something was found at or above `--fail-on`; `self-test`: a check failed |
| `2` | `hook`: the Stop gate blocked (only in `mode = "block"`) |
| `3` | usage error: unknown flag or subcommand, missing or invalid value |

The binary never exits nonzero on an internal error — a crashed or broken
leg degrades to `0`. That is what makes the hook wrapper's rule ("anything
outside `{0, 2}` means it crashed, so allow") safe.

Global behaviour:

- `--help`, `-h`, `help` print the usage text and exit 0, on any subcommand.
- A bare invocation with no subcommand is a `scan` of the whole repository.
- A missing value is a usage error, never a panic. A closed pipe on stdout
  does not panic either.

## `scan`

```text
agent-slop scan [PATHS...]
```

Walks the given paths (a file is itself; a directory is every file under it
whose extension maps to a supported language; no paths means the whole
repository root), runs the slop rules plus the eligible
[linter adapters](./linter-adapters.md), applies the ratchet when a
repository and `--since` are available, and renders.

Walk details worth knowing: hidden directories, `target` and `node_modules`
are not descended; symlinks are never followed (a `result` link from
`nix build` once pulled a whole store closure into the scan); files whose
first 8000 bytes contain a NUL are skipped as `binary`.

### Flags

`--since <ref>`
: Scan only files changed since `merge-base(ref, HEAD)` (plus untracked
  files), and ratchet against that base. An explicit ref always resolves to
  its merge base with `HEAD`, never the ref itself, so commits other people
  made on the branch are not charged to this session.

`--format human|json|jsonl|github`
: Output format; default `human` on a tty, `json` otherwise. See
  [Output formats](#output-formats).

`--severity error|warning|hint`
: Minimum reported severity; the default `hint` reports everything.

`--fail-on error|warning|hint|never`
: Exit 1 when any finding is at or above the threshold. Default `never`,
  so a scan with the default `--severity` still exits 0.

`--rules <glob,...>` / `--no-rules <glob,...>`
: Keep/drop rule ids by glob. Only a trailing `*` is supported (ids are
  `<producer>/<id>`, so prefix matching covers every selection anyone has
  asked for): `--no-rules 'slop/comment-*'`.

`--linters auto|none|<name,...>`
: Which [linter adapters](./linter-adapters.md) may run. Default `auto`:
  cost classes A and B, never an adapter that evaluates repository code.
  `none` runs only the slop rules. A named list runs exactly those adapters
  (cost classes and the repo-code rule are waived — that is what the name
  means).

`--comment-lang en|ru|auto`
: Accepted and validated, but currently unconsumed: the
  `slop/comment-language` matcher is reserved (see its rule page). The
  config key `[slop] language` is the spelling a future matcher will read.

`--strict-suppressions`
: Accepted and validated, currently unconsumed.

`--no-cache`
: Disable the fingerprint cache (`$XDG_CACHE_HOME/agent-slop/v1`) for this
  run. Session baseline state is still kept.

`--deadline-ms <n>`
: Wall-clock budget for the whole command, judge included. The default is
  the config's `timeout_ms`. When the budget expires before every path was
  scanned, the output says `truncated:` — it never fails.

`--max-verdicts <n>`
: How many comments the [judge](./judge.md) may be asked about; `0` means
  every comment. With a value passed *and* `--format human`, the scan also
  audits `needs_comment` over the diff against `--since` (or `[slop] ref`)
  — the "this code may be missing a comment" axis — appended after the
  findings.

`--emit-telemetry`
: Write one telemetry row for this scan (event `scan`). See
  [Telemetry](./telemetry.md).

`--telemetry-root <dir>`
: Where the row lands. Default: `$HOME/reviews`. (`scan` does not read
  `AGENT_SLOP_TELEMETRY_ROOT`; only `hook` does.)

Exit: `0` nothing at or above `--fail-on`, `1` something is, `3` usage
error.

## Output formats

The `--format` values for `scan`:

- `human` — one line per finding, `skipped` notes for tools and paths,
  a `truncated:` line when the deadline expired.
- `json` — a single envelope: `{"schema":1,"findings":[],"added":[],
  "suppressed":[],"skipped":[],"truncated":false}`. The judge's verdicts
  never appear here — the JSON contract is byte-identical with the model
  on or off.
- `jsonl` — one JSON finding per line.
- `github` — `::error file=…,line=…,col=…,title=<rule>::<message>` workflow
  commands, so a scan in CI annotates the diff (`error`/`warning`/
  `notice` by severity).

## `check`

```text
agent-slop check <FILE>...
```

Single advisory scan of named files: no git, no ratchet, no linter adapters
(`linters = "none"` is forced), **always exit 0**. Human output, judge
annotations included when enabled. This is the command the `PostToolUse`
advice tells the agent to run.

## `hook`

```text
agent-slop hook    # stdin: harness JSON
```

Reads one hook payload on stdin and dispatches on the event name:

| Event | Behaviour | Exit |
|---|---|---|
| `PostToolUse` | advice on the file just written, printed to stdout as a `hookSpecificOutput.additionalContext` JSON envelope; at most three advice lines | always 0 |
| `SessionStart` | pins the ratchet baseline for this session id | always 0 |
| `Stop` | ratchets against the pinned baseline; a block prints the reason to stderr | `0` allow, `2` block |

Unreadable stdin, unparsable JSON, an unknown event, no git, an expired
deadline — all exit 0. The full protocol, including the Claude/grok payload
differences and registration recipes, is
[Hooks and the ratchet](./hooks-and-the-ratchet.md).

## `explain`

```text
agent-slop explain <rule-id>
```

Prints the rule's effective level, its "do instead" note, its legitimate
exception shape, and a ready suppression line:

```console
$ agent-slop explain slop/comment-history
slop/comment-history: error
  do instead: Delete the narration; the history lives in git blame. If the code genuinely branches on the old state, say so in terms of the data ("payloads before v2 lack `kind`"), not of the edit.
  exception: A migration or compatibility shim whose behaviour DEPENDS on the old state — the comment documents a live data/format conversion, not the edit history. Name the owning decision doc to suppress.
  suppress: // agent-slop: allow slop/comment-history -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

A rule that ships `off` is still explained — its level reads `off`. An id
with no config entry at all (foreign, or unknown) prints:

```console
$ agent-slop explain slop/nope
slop/nope: no config entry (off, foreign, or unknown)
```

Imported ids (`ruff/F401`) get the note that they suppress via their own
linter mechanism (`noqa`, `allow`, `disable`), not via agent-slop lines.
A custom id also prints the file it was read from and its tier:

```console
$ agent-slop explain acme/no-placeholder
acme/no-placeholder: warning
  do instead: Implement it, or file a TODO that names the condition for removing the stub.
  exception: A stub that a test replaces with a fake on purpose, and says so.
  source: /home/me/.config/agent-slop/rules/no-placeholder.toml (user tier)
  suppress: // agent-slop: allow acme/no-placeholder -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Before the verdict, any config-degradation notes are printed to stderr
(`config note (Repo tier): …`, `rule corpus: <family> dropped, …`).

Exit: always 0.

## `rules`

```text
agent-slop rules [--format human|json|jsonl|github]   # only json differs
```

Prints every rule id and its effective level — the family table plus the
`[slop.rules.overrides]` entries on top. Without `--format`, a human table:

```console
$ agent-slop rules
slop/bare-reference          warning
slop/comment-hedging         warning
slop/comment-history         error
slop/comment-language        off
...
```

`--format json` prints the machine-readable form; custom rules carry the
`producer`, the `tier` that defined them and the file they were read from:

```console
$ agent-slop rules --format json
{"schema":1,"rules":[{"id":"acme/no-placeholder","level":"warning","producer":"acme","tier":"user","source":"/home/me/.config/agent-slop/rules/no-placeholder.toml"},{"id":"slop/comment-history","level":"error","producer":"slop","tier":"built-in","source":"embedded"}]}
```

`json` is the only format that changes the output: `human`, `jsonl` and
`github` all print the table. An invalid `--format` value or an unexpected
argument is a usage error (exit 3). Exit: 0.

## `rules lint` and `rules test`

```text
agent-slop rules lint [FILES...]
agent-slop rules test [FILES...]
```

The two commands behind custom rules (`~/.config/agent-slop/rules/*.toml`,
`<toplevel>/.agent-slop/rules/*.toml`). With no files, both act on every
file of the discovered tiers; with files, on exactly those. `lint` checks a
rule file: parse, ids, match section, fixtures, and the strict-lint bar a
promotion to `error` must clear. `test` runs each `[[fixture]]` through the
real engine:

```console
$ agent-slop rules lint
rules lint: 1 rule files checked, 0 problems

$ agent-slop rules test
ok   acme/no-placeholder fixture 1 (hit)
ok   acme/no-placeholder fixture 2 (miss)
rules test: 2 fixtures, 0 red
```

A red fixture names the rule, its tier and why it disagrees:

```console
$ agent-slop rules test
ok   acme/no-placeholder fixture 1 (hit)
FAIL acme/no-placeholder fixture 2 (miss) [user tier]: expected a miss, rule fired 1x
rules test: 2 fixtures, 1 red
```

Exit: `0` clean, `1` problems (a dropped file, a lint gap, a red fixture),
`3` usage error.

## `baseline`

```text
agent-slop baseline --since <ref>
```

Prints the baseline multiset the Stop gate would subtract — the debugging
surface for a verdict. `--since` is required; a bare `baseline` is a usage
error:

```console
$ agent-slop baseline --since main
{"fingerprints":[],"schema":1,"since":"main"}
```

Outside a repository, or for a ref that does not resolve, the list is empty:
this command diagnoses, it never fails. Exit: 0.

## `self-test`

```text
agent-slop self-test
```

The POL-026 evidence command: checks the config as it will actually run —
the *resolved* three-tier config, not the built-ins. It prints which tiers
exist, which tier last set `mode`, and then asserts:

- every known rule family has a config entry;
- the effective blocking set (`error`-level `slop/*` ids) equals the frozen
  set from SPEC-006 — a repo `.agent-hooks.toml` that promotes or demotes a
  rule past the frozen set fails here, naming the offending tier;
- `mode` was not weakened to `observe` by a higher tier while a lower tier
  said `block` (an inert gate);
- every custom rule's `[[fixture]]` cases pass through the real engine; a
  red one fails, naming the tier whose file is broken.

Custom rules promoted to `error` are listed, not failed: the frozen set
governs the embedded families only.

```console
$ agent-slop self-test
self-test: user tier /home/me/.config/agent-slop/config.toml
self-test: repo tier absent (/work/repo/.agent-hooks.toml)
self-test: mode Block from the user tier
self-test: 20 checks, 0 failed (2 custom rule fixtures run)
```

Exit: `0` all checks pass, `1` otherwise.
