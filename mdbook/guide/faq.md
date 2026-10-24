# FAQ and troubleshooting

## The Stop gate never blocks

Work down this list; each item is a real way for the gate to be inert.

1. **`mode` is `observe`.** That is the built-in default: the gate computes
   the verdict, records `would_block: true` in telemetry, and exits 0. Set
   `mode = "block"` in your user tier or in the repo's `.agent-hooks.toml`.
   `agent-slop self-test` reports which tier set `mode`, and fails with
   `mode weakened to observe by the repo tier` when a higher tier undid a
   lower tier's `block`.
2. **Nothing at `error` was added.** Only `error`-level findings block, and
   by default `slop/comment-history` is the only family at `error`
   (`agent-slop rules` shows the effective table). Warnings are reported
   in the block message of *another* error, never blocking alone.
3. **The finding is not attributable.** Without a pinned `SessionStart`
   baseline, findings in *untracked* files are reported but never block.
   Check that the `SessionStart` leg is registered and that the payload
   carries a session id (`session_id` or `sessionId`).
4. **It is inherited debt.** The ratchet only blocks *added* findings.
   `agent-slop baseline --since <ref>` prints the multiset the gate would
   subtract.
5. **A valid suppression covers it.** A suppressed finding is carried but
   does not block. An *invalid* suppression does not cover it — the finding
   shows `[suppression invalid]`.
6. **The path is excluded.** `[slop.exclude]` applies to the gate as well
   as to `scan`.
7. **Re-entry or a subagent turn.** `stop_hook_active: true`, a non-`end_turn`
   `reason`, or a `subagent_type` in the payload each skip the scan by
   design.
8. **Not a git repository.** Stderr says `agent-slop: not a git repository,
   ratchet off`.
9. **The hook is not registered or not firing.** Test the binary directly:

   ```sh
   printf '%s' '{"hook_event_name":"Stop","session_id":"x"}' | agent-slop hook; echo $?
   ```

   and run `crates/agent-slop/tests/stop_gate_blocks.sh <binary>` to see
   the gate fail and pass in a scratch repository. If your harness is grok
   and nothing happens, make sure you did not copy a Claude matcher onto
   `SessionStart` — grok's start source is `"new"`, and an omitted matcher
   is the one that matches it.

## Everything exits 0 even when I expect 1

`scan` exits 0 unless `--fail-on` says otherwise: the default is `never`.
Add `--fail-on error` (or `warning`, `hint`) to turn findings into exit 1.
`check` and the `PostToolUse` leg *always* exit 0.

## `scan` prints JSON instead of text

The default `--format` is `human` on a tty and `json` otherwise. Pass
`--format human` explicitly when piping.

## A finding I think is wrong

Run `agent-slop explain <rule-id>`: it prints the legitimate-exception shape
and a ready suppression line. If the comment really is the exception, add a
suppression with a referent:

```python
# agent-slop: allow slop/comment-history -- migration shim owned by ADR-031
```

If the rule is simply wrong for your project, tune it instead of
suppressing line by line:

```toml
[slop.rules]
comment-paraphrase = "off"
```

If you believe the phrase list is wrong, the fix belongs in the corpus
under `crates/agent-slop-core/rules/` with a provenance-backed fixture —
see [Contributing](./contributing.md#adding-a-rule).

## My suppression is flagged

`slop/suppression-empty` — no `-- <reason>`, or fewer than 12 non-whitespace
characters of reason. `slop/suppression-vacuous` — the reason has no
referent (URL, `ADR-064`-style id, `owner/repo#1`, a path with a slash, or
`date:YYYY-MM-DD`). Words alone ("because I say so") never count. See
[Suppressions](./suppressions.md).

## A warning about config

Config problems are printed on stderr, never fatal:

- `config note (Repo tier): config dropped, malformed TOML or wrong types:
  …` — the whole tier was dropped, not just the bad key; lower tiers
  stand. Fix the TOML (a string where an integer belongs counts).
- `config note (…): unknown rule name in [slop.rules]: typo-family (kept,
  but no such rule exists in v1)` — likely a typo in a family name.
- `config note (…): config dropped, unreadable: …` — the path exists but
  cannot be read (permissions, or it is a directory).
- `rule corpus: <family> dropped, …` — an embedded rule file failed to
  parse. Unreachable in a shipped binary (the tests check every file
  parses); the message exists because the fail-open matrix promises the
  drop path.

## `scan` found nothing in my files

- The extension must map to a supported language: `rs`, `java`, `py`/`pyi`,
  `go`, `ts`/`mts`/`cts`, `tsx`, `js`/`mjs`/`cjs`/`jsx`, `sh`/`bash`, `nix`,
  `c`/`h`, `cc`/`cpp`/`cxx`/`hpp`/`hh`, `yaml`/`yml`. Markdown, TOML and
  JSON are not scanned.
- Hidden directories, `target/` and `node_modules/` are not walked; symlinks
  are never followed.
- Check `skipped[]` in `--format json`: `excluded`, `binary`, `unreadable`.
- The family may be `off` — `comment-language` and `rejected-alternative`
  ship off.
- `--since` narrows the scan to files that differ from the merge base.
- A `truncated` field of `true` means `timeout_ms` (default 4000) expired
  before every path was scanned: raise `--deadline-ms` or `timeout_ms`.

## A linter I expected did not run

Look at the `skipped` lines. `absent` means the binary is not on `PATH`;
`no-targets` means no scanned file has an extension that tool handles.
`clippy` and `tsc` never run under `--linters auto` — they evaluate
repository code and must be named: `--linters clippy,tsc`. The Stop gate
runs no linters at all. See [Linter adapters](./linter-adapters.md).

## No telemetry rows appear

Telemetry is opt-in. For `scan`: `--emit-telemetry` (and `--telemetry-root`).
For hooks: `AGENT_SLOP_TELEMETRY_ROOT`, or the module's
`programs.agent-slop.telemetry.enable = true` — with it off the wrapper
points the root at `/dev/null/agent-slop`, which fails with `ENOTDIR` on
purpose. Writes fail silently; if rows still don't appear, check that the
parent directory is writable.

## The `laya:` lines never appear

The judge is off by default (`[slop.judge] enable = true`), needs an HTTP
endpoint on the configured `url` (loopback, plain `http://`), and prints
nothing when the model is unreachable, slow, or below `min_confidence`
(default 0.9). All of that is by design — see [The optional
judge](./judge.md).

## `nix build` fails in the install check

The package's install check runs `stop_gate_blocks.sh` and requires five
named lines. A failure there means the *built binary's Stop gate does not
behave*; it is a real regression, not flakiness. The script's output is
printed on failure.

## I moved a file and the gate noticed / didn't notice

Fingerprints exclude the path on purpose, so a moved violation is invisible
and a moved file does not turn inherited debt into "added". A comment copied
into a second file *is* seen: baselines are multisets with multiplicity.
See [Hooks and the ratchet](./hooks-and-the-ratchet.md#fingerprints-and-the-multiset).

## Where state lives

- `~/.config/agent-slop/config.toml` — user tier;
- `<git toplevel>/.agent-hooks.toml` — repo tier;
- `$XDG_CACHE_HOME/agent-slop/v1/` (or `~/.cache/agent-slop/v1/`) —
  fingerprint cache; safe to delete;
- `$XDG_CACHE_HOME/agent-slop/session/` — pinned baselines, swept after
  seven days; deleting one makes Stop fall back to `[slop] ref`;
- `<root>/<project>/agent-feedback/…` — telemetry, only when enabled.

Nothing is ever written inside the repository.
