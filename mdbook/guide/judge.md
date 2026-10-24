# The optional judge

The judge is a *second verdict*: a model annotation printed as context next
to a finding. It is **off by default** — every `[slop.judge]` field
defaults to a value that annotates nothing — because the endpoint is a
local model this machine may or may not be running.

## What it is

The rules are lexical: `slop/comment-history` fires on a phrase, not on
whether the comment is actually doing its job. Laya is a `ModernBERT`
classifier fine-tuned on 11k real review remarks, served by laya.cpp's
`POST /v1/systemone` endpoint on loopback. It answers two questions:

- **verdict** — should this comment stay as written? `delete`, `rewrite` or
  `keep`;
- **needs_comment** — does this bare code warrant a comment it does not
  have? `yes` or `no`.

Measured reliability of the served checkpoints, because the number belongs
next to the feature:

| axis | measurement |
|---|---|
| verdict, 3-way choice | 0.628 on 250 held-out rows; always-keep scores 0.664 |
| verdict, delete-vs-keep | 0.700 binary, against a 0.656 always-keep baseline |
| `needs_comment` | AUROC 0.832–0.871 on held-out rows |

The verdict head also inherits a length confound from its training data
(kept comments: median 44 words; deleted: 17), so it calls a short,
load-bearing "why" comment `delete` at p≈0.9. That is why the confidence
floors (`min_confidence`, `needs_min_p`) exist and default high at 0.9: a
verdict below them prints nothing.

## The contract: additive only, never a verdict

A verdict is allowed to do exactly one thing: **print**. It never changes a
severity, a fingerprint, a ratchet decision, an exit code or a
suppression. The deterministic finding stands exactly as the rule produced
it; the model answer is one more line of context for whoever reads the
output. A verdict that disagreed with the rule could have demoted the
finding; it does not, because a classifier below the majority baseline on
its own three-way choice is not evidence enough to overrule a rule. A user
decision (2026-09-30) froze this: additive only.

Concretely:

- the JSON envelope is byte-identical with the judge on or off — verdicts
  are consumed by the human renderer only;
- a block verdict is computed before the annotation is rendered; a slow or
  stopped model costs the annotation line, never the gate;
- a confident `needs_comment` **no** is not printed under a "may be missing
  a comment" header — the floor decides whether to speak, the answer
  decides what to say.

## Where verdicts appear

- **`scan` / `check`** — under each eligible finding, indented:
  `laya: delete 0.91 (delete 0.91 / rewrite 0.02 / keep 0.07)`.
- **`PostToolUse`** — the same annotation under the advice line.
- **`Stop`** — under blocking findings in the block message.
- **`needs_comment`** — only where a diff is available: the `PostToolUse`
  leg asks about comment-free runs of added lines in the file just written,
  and `scan --max-verdicts <n> --format human` audits the whole diff
  against `--since` (or `[slop] ref`), printing
  `file:line laya/needs-comment (advisory) — this code may be missing a comment`.
  These are not rule findings: nothing matched, nothing is suppressed,
  nothing blocks.

Only the eleven comment-form families are asked the verdict question —
`todo-no-condition` is deliberately absent: that rule's defect is a missing
condition, and asking "should this comment stay" about it produces a
confident answer to a question nobody asked.

## Configuration

```toml
[slop.judge]
enable = true
url = "http://127.0.0.1:8095/v1/systemone"   # the gateway port, not the model port
timeout_ms = 2500
max_verdicts = 4
min_confidence = 0.9
needs_min_p = 0.9
```

Every key with its default is in
[Configuration](./configuration.md#slop-judge). Budget notes:

- `timeout_ms` is the wall clock for the whole leg, not per request — the
  judge shares its caller's budget rather than adding one, so switching the
  model on cannot make a scan overstay the time its caller allowed;
- `max_verdicts` caps requests per leg. A file with 200 comments must not
  become 200 round trips inside one tool call. On the CLI,
  `--max-verdicts 0` means every eligible comment (an audit run); reading
  `0` as a cap of zero would be the quietest possible bug — the run would
  finish and print nothing;
- use the *gateway* port: the gateway stays up when the model is stopped
  and answers 503, which is the failure the judge is built to absorb.

## Failure is silent and normal

Everything is best-effort by construction: no endpoint, a refused
connection, a timeout, a 503 from a stopped model, a malformed body, a
truncated option list — each returns "no verdict" and prints nothing. Why a
request produced nothing is never printed: the annotation is an
enrichment, and an unreachable model must not add noise to a gate. The Stop
gate has 15 s and must work offline; a model that is not running is a
normal state, not an error.

Two wire-format details are load-bearing and were measured against the
served checkpoints, not assumed:

- a choice option is rendered as `"key: description"`, matching the
  training-time option renderer; sending bare keys or bare descriptions
  moved 11 of 25 verdicts on identical comments;
- the `Repo:` line of the prompt state carries no provenance parenthetical:
  the training states all said `(elite)`, which is a fact about the mined
  corpus, not about the repository being scanned. Dropping it costs about
  two points of choice accuracy (0.628 → 0.608) — the price of not
  asserting something untrue to the model.
