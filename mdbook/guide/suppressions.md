# Suppressions

A suppression is a comment that tells agent-slop a specific finding is
accepted here, and why. The syntax is
`<marker> agent-slop: allow <rule-id> -- <reason>`, **line-scoped** — there
are no file-level or block-level suppressions in v1.

## Syntax

```python
# agent-slop: allow slop/comment-history -- rollback steps required by ADR-031
# We previously always sent a Referer header here.
referer = None
```

Two placements exist:

- **Standalone**: the allow line is a comment of its own and suppresses the
  finding on the *next non-empty line*. A standalone allow line is skipped
  as content — it never becomes a finding of its own and never joins a
  comment paragraph.
- **Trailing, same line**: the directive sits at the end of the comment it
  suppresses, conventionally in parentheses:

  ```python
  # For now serial-only (agent-slop: allow slop/comment-meta -- bounded by ADR-041)
  x = 1
  ```

Markers are the usual comment introducers: `//`, `#`, `/*`, `--`, `;` — use
the one the file's language speaks. The rule id must be a real
`<producer>/<id>`: `<rule-id>` and `slop/...` as literal placeholders do not
parse, which is what keeps prose *about* the syntax from being a directive
(the scanner reads its own source, where the syntax is documented).

## The reason is mandatory, with a referent

An invalid suppression is not silently accepted — it raises its own finding
at `error`:

- no `-- <reason>`, or a reason shorter than 12 non-whitespace characters →
  `slop/suppression-empty`;
- a reason without a **referent** → `slop/suppression-vacuous`.

A referent is one of:

- a URL (`https://…`);
- a decision-doc id: `ADR-064`, `POL-024`, `SPEC-006`, `INC-003`, `TASK-019`
  (sentence punctuation around it is fine: `(ADR-064:)`);
- an issue ref: `owner/repo#123` or `#123`;
- a path containing a slash: `docs/decisions/adr-064.md`;
- a date: `date:2027-01-01`.

```console
$ agent-slop check demo.py
demo.py:1:1 slop/suppression-vacuous — invalid suppression: because I say so okay — reason must carry a referent (URL, ADR-064, owner/repo#1, path/to, date:2026-09-21)
demo.py:2:1 slop/comment-history — change-history residue 'previously' — narrates the edit, not the code [suppression invalid]
```

Real output for each failure mode, from the binary:

| Suppression line | Result |
|---|---|
| `# agent-slop: allow slop/comment-history -- rollback steps required by ADR-031` | valid; the finding is `[suppressed]` |
| `# agent-slop: allow slop/comment-history` | `slop/suppression-empty` ("no `-- <reason>` given"); finding `[suppression invalid]` |
| `# agent-slop: allow slop/comment-history -- because I say so okay` | `slop/suppression-vacuous`; finding `[suppression invalid]` |
| `# For now serial-only (agent-slop: allow slop/comment-meta -- bounded by ADR-041)` | valid, same line |

Only a matching rule id suppresses: an allow line for `slop/comment-meta`
does nothing to a `slop/comment-history` finding.

## Suppressed findings are carried, never dropped

A suppressed finding keeps its place in the output with a
`[suppressed]` marker, and in the JSON envelope its suppression moves to the
`suppressed[]` array (`{"rule", "reason", "valid"}`). Telemetry counts both
directions: a per-rule histogram of findings and a separate
`slop/suppressed:<family>` entry carrying the reason (truncated to 160
characters). "Which rules get silenced, and why" stays observable — see
[Telemetry](./telemetry.md).

Because an *invalid* suppression still leaves the finding in place
(`[suppression invalid]`), a `slop/*` finding at `error` that the Stop gate
would block on stays blocking until the suppression is valid — the gate
only accepts suppressions whose `valid` is true.

## Scope

Line-scoped only. The two shapes above are the whole grammar: no
file-level, function-level or block-level suppression exists, and there is
no `agent-slop: allow-all`. A rule you never want in a repository is
disabled in [configuration](./configuration.md#slop-rules), not suppressed
per line.

Imported linter findings (`ruff/F401`, `deadnix/dead-code`, …) are not
suppressed by agent-slop lines; they suppress via their own linter's
mechanism (`noqa`, `#[allow]`, `// nolint`, …), which
`agent-slop explain <id>` also says.
