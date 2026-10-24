# Telemetry

agent-slop can append one JSONL row per scan or Stop gate run to an
existing feedback shard, so there is no new subtree, no new store, and no
change to any exit code: every write failure is swallowed and the gate's
verdict never depends on the recording of it.

## Opt-in and where it lands

Off by default. Three ways to switch it on:

- the CLI: `agent-slop scan --emit-telemetry [--telemetry-root <dir>]`;
- the hook: the environment variable `AGENT_SLOP_TELEMETRY_ROOT` (only the
  `hook` leg reads it; `scan` uses its flag or the `$HOME/reviews` default);
- the home-manager module: `programs.agent-slop.telemetry` (see the
  [option reference](./installation.md#option-reference)). With telemetry
  disabled the module's wrapper exports
  `AGENT_SLOP_TELEMETRY_ROOT=/dev/null/agent-slop` so the binary's default
  (`$HOME/reviews`) can never receive rows by accident.

Default root is `$HOME/reviews`; the shard path is
`<root>/<project>/agent-feedback/<YYYY>/<MM>/<YYYY-MM-DD>.jsonl`, written
with one `O_APPEND` line under the page size — atomic on Linux, the same
guarantee the shell writers of the same shard rely on. Multiple producers
(harness recorders, agent-slop) share one shard by design.

`<project>` is the digest's project key, computed in rule order:

1. `cwd == $HOME` → `"home"` (a dotfiles checkout in `~` must not resurrect
   the username as a project name);
2. a relative cwd → its basename, no walk;
3. the ephemeral realms — `/tmp`, `/var/tmp`, `$TMPDIR`, `~/.grok`,
   `~/.cache` → `"ephemeral"`, decided *before* the walk so a degraded floor
   can never escape into a repo outside the realm;
4. otherwise the nearest enclosing repository: a `.git` *directory* is a
   repo (a nested repo never folds into an outer one); a `.git` *file* folds
   into its repo only when the pointer has the linked-worktree shape
   (`…/.git/worktrees/<name>`), a submodule keeps its own basename. The
   walk stops at `$HOME` for paths under it, at `/` elsewhere, and falls
   back to the cwd's basename. One deliberate tension: a live worktree under
   `~/.grok/worktrees` lands in `"ephemeral"`, not its parent repo —
   fold-then-escape is the failure the rule order exists to kill. The row
   keeps the raw `cwd` so offline re-attribution stays possible.

Unsafe project names (anything outside `[A-Za-z0-9._-]`) become
`"unknown"`.

## The row

`record_kind` is `slop_scan`, `schema` is `2`. Field by field (the fixed
values are load-bearing for the consuming miner):

| Field | Value / meaning |
|---|---|
| `ts`, `date` | UTC timestamp and `YYYY-MM-DD` (civil-from-days math, no date crate) |
| `event` | `"scan"`, `"PostToolUse"`, or `"Stop"` |
| `harness` | `"grok"` when `GROK_SESSION_ID` is set, else `"claude"` |
| `session_id`, `parent_session_id`, `subagent_type` | from the hook payload (`scan` uses `GROK_SESSION_ID` when present) |
| `cwd`, `project`, `date` | attribution, as above |
| `severity` | always `"none"` — slop is not a tool failure, so the consumer's environment-problem counters stay off |
| `human_action_required` | always `false` |
| `defects[]` | see below |
| `telemetry.mode` | `"observe"` or `"block"` |
| `telemetry.ref` | `"none"` (PostToolUse), `"session"` (pinned baseline), or the ref a scan compared against |
| `telemetry.blocked`, `telemetry.would_block` | what the gate did and what it would have done; `would_block` is the observe-mode evidence — without it a observe-only rollout records nothing about blocking at all |
| `telemetry.files_scanned`, `telemetry.duration_ms` | scale of the run |
| `telemetry.findings_total`, `telemetry.suppressions_total` | counts |
| `telemetry.linters` | per-tool status map: `{"ruff": "ok"}`, `"statix": "skipped:absent"`; empty on the Stop leg, which runs no linters |
| `self_report`, `human_annotation`, `mined` | `null` — filled by the consumer's pipeline, not by this producer |

A real Stop row, pretty-printed:

```json
{
  "record_kind": "slop_scan",
  "schema": 2,
  "event": "Stop",
  "harness": "grok",
  "session_id": "s1",
  "project": "ephemeral",
  "severity": "none",
  "human_action_required": false,
  "telemetry": {
    "mode": "block",
    "ref": "session",
    "blocked": true,
    "would_block": true,
    "files_scanned": 1,
    "duration_ms": 7,
    "findings_total": 1,
    "suppressions_total": 0,
    "linters": {}
  },
  "defects": [
    {
      "category": "agent_behaviour",
      "item": "slop/comment-history",
      "detail": "slop/comment-history 1x in 1 files, top 'a.py:1' [agent-slop scan --since session]",
      "source": "agent-slop",
      "count": 1,
      "nix_surface": "pkgs/agent-slop/rules/ (rule corpus)"
    }
  ]
}
```

## `defects[]`

Each entry is `category: "agent_behaviour"` with `source:
"agent-slop"`:

- one entry per rule with added findings: `item` is the bare rule id
  (`slop/comment-history`), `detail` names the count, the file count, the
  top location and the command that reproduces the evidence
  (`agent-slop scan --since <ref>`, or `agent-slop check <file>` when the
  leg compares against no ref);
- one `slop/suppressed:<family>` entry per rule with suppressed findings,
  carrying the suppression reason (truncated to 160 characters) and location;
- one `slop/gate-blocked` entry when the gate actually returned exit 2.

The list is capped at 20 entries (the consumer weights sessions by defect
count, so an unbounded row would dominate the digest); overflow collapses
into a `truncated` entry.

## What is never recorded

**The comment text.** No snippet, no payload, no reason text beyond the
suppression reason an author wrote for the repo. This store keeps data
forever, and a finding's snippet is someone's source code.

## Recorder integration

A session recorder that owns the same shard rewrites a session's rows at
`SessionEnd`, keyed on `harness × session_id`. Its jq filter keeps the
session's rows, so the filter carries an explicit `record_kind ==
"slop_scan"` carve-out — without that clause the rewrite would delete every
row this producer wrote, which is the state agent-slop shipped into before
the carve-out landed. The compatibility check is an opt-in end-to-end test
(`crates/agent-slop/tests/telemetry.rs`, run with `AGENT_SLOP_FEEDBACK_RECORD`
pointed at the consumer's `feedback-record.sh`): it reads the exact jq
program out of the shipped script and asserts the row survives the rewrite
*and* that removing the clause removes the row.

The home-manager module can register a recorder next to the agent-slop legs:
`programs.agent-slop.telemetry.recorder.command`, with per-harness event
lists (`claudeEvents` default `["SessionEnd" "PostToolUseFailure"
"StopFailure" "PermissionDenied"]`, `grokEvents` default
`["StopCancelled"]`).
