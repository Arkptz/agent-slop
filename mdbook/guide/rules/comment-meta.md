# `slop/comment-meta`

**Built-in level:** `warning`

## What it catches

Process residue addressed to the reader of a *session* rather than of the code: "as requested", "for now", "left as-is", "temporarily disabled", and Russian ("по запросу", "пока так", "на время"). A deferral without an owner is a decision nobody can audit. Demoted from `error` to `warning` on 2026-10-03 (TASK-019): 28 firings in the harvest window with zero would-block events, and the Russian phrases also match ordinary prose about timing.

## Legitimate exception

> A comment naming the decision document that owns the deferral (ADR/POL/SPEC/INC/TASK id, an issue ref, or a date-scoped rollback window). The deferral is owned, not orphaned.

## Do instead

Replace the session-residue with the durable fact, or delete it. 'For now' must become 'until `<referent>`' — see the exception shape; anything else is a promise with no owner.

## Example

Bad:

```python
# For now this stays serial.
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/comment-meta — session residue 'for now' — deferral with no owner
```

Good:

```python
# Serial until the worker pool lands (ADR-041).
x = 1
```

The good version produces no finding for this rule.

## Match configuration

- `word_boundaries` = `true`

<details>
<summary><code>phrases</code> (24 entries)</summary>

```text
for now
as requested
as discussed
as per your
per your request
left as-is
left as is
left unchanged for
temporarily disabled
temporarily commented
по требованию
по запросу
пока так
пока не
пока остав*
пока что
на время
временно отключ*
временно закомментир*
как договорились
как просил*
по просьбе
по твоей просьбе
по вашей просьбе
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
comment-meta = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/comment-meta -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
