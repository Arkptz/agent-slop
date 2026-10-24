# Hooks and the ratchet

`agent-slop hook` is the integration surface for coding-agent harnesses. It
reads one hook payload on stdin, dispatches on the event name, and
implements three legs that together form a ratchet: advice while the agent
works, a baseline pinned when the session starts, and a gate when the turn
ends.

## The contract

- The **only** nonzero exit the hook may produce is the deliberate Stop
  block, exit `2`. Unreadable stdin, unparsable JSON, an unknown event, no
  git, an expired deadline, a failed telemetry write, a crashed judge: all
  exit `0`.
- A hook writes nothing to the repository. Pinned session state and the
  fingerprint cache live under `$XDG_CACHE_HOME/agent-slop` (falling back to
  `~/.cache/agent-slop`).
- Every leg has a budget: `timeout_ms` (default 4000) from the config,
  separate from the harness's own timeout (10 s for `PostToolUse`, 15 s for
  `SessionStart` and `Stop` in the shipped registrations).

## Legs

### `PostToolUse` — advisory, always exit 0

Fires after a write tool. If the payload carries a file path (a `Bash` call
edits no file and returns immediately), the leg reads the file as it stands
on disk and runs the slop rules on it. No git, no baseline, no ratchet: it
reports the file's current state.

Findings carrying a suppression are dropped from the advice. Up to three advice lines
are returned, `file:line rule — message`, followed by
`+N more — run agent-slop check <file>` when there are more. The advice
reaches the model as `hookSpecificOutput.additionalContext` on stdout at
exit 0:

```console
$ printf '%s' '{"hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"a.py"},"session_id":"s1"}' | agent-slop hook
{"hookSpecificOutput":{"additionalContext":"a.py:1 slop/comment-history — change-history residue 'previously' — narrates the edit, not the code","hookEventName":"PostToolUse"}}
```

Exit `2` is rejected for advice on purpose: it keeps a reason but *drops*
`additionalContext`, and `2` is reserved for the Stop gate. With the
[judge](./judge.md) enabled, the leg may also add `laya:` annotation lines
and an advisory `laya/needs-comment` block for the code the agent just
added — still exit 0, still no severity.

Every scanned file produces a telemetry row (event `PostToolUse`), findings
or not, so the absence of the leg is observable.

### `SessionStart` — pins the baseline, always exit 0

Pins the baseline for the payload's session id: the `HEAD` commit and the
multiset of fingerprints of every *untracked* file's findings, written to
`$XDG_CACHE_HOME/agent-slop/session/<session-id>.json`:

```json
{"schema":1,"commit":"994f5733e0bc3833ef07df88dbf70cb59553c465","untracked":{},"written_at":1792884587}
```

Prints nothing; `SessionStart` cannot block. Without a session id or outside
a repository, it does nothing. A session id that is not a plain identifier
(`[A-Za-z0-9_-]+`) is refused rather than sanitised — a silently rewritten
id reads back as a missing session, which is the wrong failure. Pinned
files older than seven days are swept on read.

### `Stop` — the gate, exit 0 or 2

Computes `added = multiset(current) \ multiset(baseline)` over the files the
session changed, and blocks only when the gate's blocking set is non-empty
*and* `mode = "block"`.

Three preconditions short-circuit to exit 0 without scanning at all:

1. `stop_hook_active` is true — the agent is already continuing because a
   stop hook blocked earlier this turn; the verdict cannot change until it
   edits something, so re-blocking is a loop.
2. The payload carries a `reason` other than `end_turn` — a session-end Stop
   has no turn left to continue. Claude Code sends no reason at all, so
   *absence* does not skip the scan.
3. The payload names a `subagent_type` — a subagent's turn is not the
   session's turn.

If there is no git repository, the leg prints
`agent-slop: not a git repository, ratchet off` on stderr and exits 0.

A block prints the reason on **stderr** and exits `2`. Every added error is
rendered in full with its exception and a ready suppression line; the render
ceiling is five findings per rule, then `+N more <rule>`; added
warnings/hints collapse to one `+N added warnings/hints — run agent-slop
scan --since HEAD` line:

```console
$ agent-slop hook < stop.json
agent-slop: this turn added slop that blocks (ADR-064 ratchet)
  a.py:1:1 slop/comment-history — change-history residue 'previously' — narrates the edit, not the code
    legitimate when: A migration or compatibility shim whose behaviour DEPENDS on the old state — the comment documents a live data/format conversion, not the edit history. Name the owning decision doc to suppress.
    instead: Delete the narration; the history lives in git blame. If the code genuinely branches on the old state, say so in terms of the data ("payloads before v2 lack `kind`"), not of the edit.
    suppress: agent-slop: allow slop/comment-history -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
$ echo $?
2
```

A block the agent cannot resolve teaches it to route around the gate, so
each error carries the rule's exception shape and a pasteable suppression.

In `observe` mode (the built-in default) the identical computation runs and
the row records `would_block: true`, but the leg exits 0. That separation is
deliberate: it lets a rollout gather evidence about blocking before the
gate is switched on.

## What can block

An added finding blocks only if all of these hold:

- its severity (after config) is `error`. By default that is
  `slop/comment-history` alone; the suppression meta-rules
  (`slop/suppression-empty`, `slop/suppression-vacuous`) also report at
  `error`;
- it is not validly suppressed;
- it is *attributable*: with a pinned session baseline, any added finding is.
  Without one (see below), findings in untracked files are reported but
  never block;
- `mode = "block"`.

Imported linter findings never reach the Stop gate — the Stop leg runs no
linters, because its baseline is read from git blobs and a linter cannot
lint those without materialising a tree.

## Fingerprints and the multiset

The ratchet's identity for a finding is its **fingerprint**: the first eight
bytes of `blake3(rule_id ‖ 0x00 ‖ normalize(snippet))`, as sixteen lowercase
hex characters. `normalize` strips the leading comment markers (anchored —
`/**`, `///`, `//!`, `//`, `/*`, `#`, `--`, `;` — never a global replace,
which would eat the `--` suppression separator and every SQL-style `--`),
collapses whitespace runs including newlines to one space, trims, and
applies Unicode NFC (Russian "й" has composed and decomposed spellings;
without NFC one comment would have two fingerprints).

The path is **excluded** from the hash on purpose. Consequences, all
intended:

- moving or renaming a file does not turn inherited debt into "added";
- reflowing or re-wrapping a comment, or switching `//` to `/* */`, is not
  new slop;
- copying an existing slop comment into a second file *is* seen — baselines
  are multisets with multiplicity, so the count moves 1→2;
- deleting a violation never blocks;
- a violation *moved* between files is invisible: the ratchet counts debt, it
  does not localise it (an accepted failure);
- the truncated hash can collide, and collisions are strictly fail-open.

`agent-slop baseline --since <ref>` prints the multiset for debugging a
verdict.

## The baseline and its fallbacks

The baseline is *computed, never stored in the repository* — a checked-in
baseline rots, conflicts on merge, and needs a regeneration command an
agent would run to silence the gate. Two measured facts shape the design:

- An agent commit mid-session empties `git diff --name-only HEAD`, which
  would make the session's own slop its baseline. So the commit is pinned
  once at `SessionStart` and the Stop leg diffs against *that* commit, not
  `HEAD`.
- Findings inherited in an untracked file would count as added forever
  against an empty baseline. So `SessionStart` also pins the fingerprints of
  every untracked file's findings, and Stop adds them to the baseline.

At Stop, the baseline base commit is chosen in order:

1. the pinned session commit — recorded as `ref: "session"` in telemetry;
2. otherwise `[slop] ref` resolved against `HEAD` (`auto` = merge base with
   the first of `origin/HEAD`, `main`, `master`) — the degraded
   `live-head` path.

`agent-slop scan --since <ref>` is the command-line twin: it resolves the
ref to its merge base with `HEAD` and computes the same `added` multiset,
without any pinned session state.

Without pinned state (`live-head`), findings in untracked files cannot be
told from inherited debt, so they are *reported as added but never
blocking*. "Unattributable debt never blocks."

The baseline side of the diff is read from git blobs (`git show
<commit>:<path>`); a path absent at the base contributes nothing, which is
what makes a file the session created fully attributable to it. The
worktree side reads the files as they stand on disk — what the agent wrote
is what is scanned, whether or not it is staged.

Paths that contribute nothing are recorded, not hidden: `excluded`,
`missing`, `symlink`, `submodule`, `unreadable`, `binary`, `deadline`.

### The cache

Baseline fingerprints are cached under
`$XDG_CACHE_HOME/agent-slop/v1/<key>.json`, one small JSON array of
fingerprints per `(ruleset version, file extension, content hash)` key.
Blobs are immutable and a rule change changes the key, so an entry is valid
forever. The *ruleset version* is a hash of the package version plus the
embedded rule files, so a rebuild with different rules invalidates
everything rather than serving old fingerprints. `--no-cache` on `scan`
disables writing and reading these entries; session state is unaffected.

## Exit-code matrix

| Situation | Exit |
|---|---|
| unreadable stdin / unparsable JSON / unknown event | `0` |
| `PostToolUse` with no file path (`Bash`) or an unreadable file | `0` |
| `PostToolUse`, any findings | `0` (advice on stdout) |
| `SessionStart`, any outcome | `0` |
| `Stop`, `stop_hook_active`, session-end reason, or subagent | `0` |
| `Stop`, not a git repository | `0` + stderr note |
| `Stop`, nothing added at `error`, or `mode = "observe"` | `0` |
| `Stop`, `mode = "block"` and an attributable added `error` | `2`, reason on stderr |
| anything else the binary can hit (panic, internal error) | the wrapper maps it to `0` |

## Claude Code and grok payloads

The two harnesses send different envelopes and the binary understands both.

| Field | Claude Code | grok |
|---|---|---|
| event | `hook_event_name: "PostToolUse"` | **both** `hook_event_name: "PostToolUse"` and `hookEventName: "post_tool_use"` in one object |
| tool | `tool_name`, `tool_input.file_path` | both spellings: `tool_name`/`toolName`, `tool_input`/`toolInput`, `file_path`/`filePath` |
| session | `session_id` | both `session_id` and `sessionId` |
| Stop re-entry | `stop_hook_active` | `stopHookActive` (and `stop_hook_active`) |
| Stop reason | none (absence does not skip) | `reason: "end_turn"` on a normal Stop |
| write tools | `Edit`, `Write`, `MultiEdit`, `NotebookEdit` | `write`, `search_replace`, `edit_notebook` |
| `SessionStart` source | `startup`, `resume`, `clear`, `compact` | `"new"` — no Claude matcher value matches it |

Three things follow from this and are encoded in the binary:

- **Both spellings are declared as separate fields, never as a serde
  alias.** grok sends a canonical key and its alias in the same object,
  which serde rejects as `duplicate field`; under an alias the whole parse
  fails and every leg becomes a silent no-op at exit 0.
- **Event names are compared folded.** grok's camelCase key carries a
  snake_case value (`post_tool_use`) while its snake_case key carries
  Claude's `PostToolUse`. The compare lowercases and drops `_` and `-`, so
  `PostToolUse`, `post_tool_use` and `posttooluse` are one event.
- **Telemetry harness detection** is `GROK_SESSION_ID` set → `grok`, else
  `claude`.

## Registering the hook

### With the home-manager module

The module registers the three legs for you; see
[Installation](./installation.md#registered-legs) for the exact matchers and
timeouts. grok is written to `~/.grok/hooks/agent-slop.json`; for Claude
you merge `programs.agent-slop.claudeHooks` into your own `settings.json`
wiring:

```nix
{ config, ... }: {
  programs.claude-code.settings.hooks =
    config.programs.agent-slop.claudeHooks;
  # If you already define hooks, concatenate per-event lists instead:
  # lib.zipAttrsWith (_: lib.concatLists) [ existing config.programs.agent-slop.claudeHooks ]
}
```

### By hand: Claude Code

Add to Claude's `settings.json`, where `agent-slop-hook` is a wrapper (below)
or the plain binary invoked as `agent-slop hook`:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [{ "type": "command", "command": "agent-slop hook", "timeout": 10 }]
      }
    ],
    "SessionStart": [
      {
        "matcher": "startup|resume|clear|compact",
        "hooks": [{ "type": "command", "command": "agent-slop hook", "timeout": 15 }]
      }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "agent-slop hook", "timeout": 15 }] }
    ]
  }
}
```

### By hand: grok

Write `~/.grok/hooks/agent-slop.json`. Note the different tool names and the
absent `SessionStart` matcher:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "write|search_replace|edit_notebook",
        "hooks": [{ "type": "command", "command": "agent-slop hook", "timeout": 10 }]
      }
    ],
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "agent-slop hook", "timeout": 15 }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "agent-slop hook", "timeout": 15 }] }
    ]
  }
}
```

### Other harnesses

Any harness that can run a command on those three events and pass the JSON
payload on stdin works. Rules of thumb:

- point it at `programs.agent-slop.hooks.command` if you use the Nix
  module, or write a three-line wrapper yourself;
- if the harness treats a nonzero exit as "block", wrap the binary so that
  only `0` and `2` pass through and everything else becomes `0` — the
  module's wrapper does exactly this:

  ```sh
  agent-slop hook
  rc=$?
  case "$rc" in
    0 | 2) exit "$rc" ;;
    *) exit 0 ;;
  esac
  ```

- the event name may be spelled `PostToolUse`, `post_tool_use` or any case
  variant; `Stop` and `SessionStart` likewise;
- the payload needs a `session_id`/`sessionId` for the ratchet to pin and
  find its baseline; without one `SessionStart` is a no-op and Stop falls
  back to `[slop] ref`.

### Smoke-testing the gate

The package build proves the gate can fail:
`crates/agent-slop/tests/stop_gate_blocks.sh <binary>` drives the real `hook`
leg against scratch git repositories and asserts both directions — exit 2
on a dirty tree under `mode = "block"`, exit 0 on a clean one — plus the
grok envelope, an excluded path, and an ordinary `NOTE:` heading that is not
a review finding. Run it against any binary you are about to wire in.
