# `slop/comment-steps`

**Built-in level:** `warning`

## What it catches

Numbered step narration ("Step 1: fetch the token", "Шаг 2.", "Этап 3") frozen into source. Steps one through eight are covered. The match is *anchored to the comment's start*: step narration announces itself, while protocol quotes cite the step mid-sentence and stay out. The numbering restates control flow the reader can follow, rots the first time the order changes, and teaches the next agent that narrating is wanted here.

## Legitimate exception

> A protocol or spec quote whose numbering is external and load-bearing ('Wire step 2: the server sends ACK') — the numbers belong to the protocol, not to this code's control flow.

## Do instead

Delete the numbering and keep only the non-obvious WHY of the step, if there is one. If the sequence itself is the information, extract a named function per step so the call site reads as the sequence.

## Example

Bad:

```python
# Step 1: fetch the token
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/comment-steps — step narration 'step 1' — the control flow is the code
```

Good:

```python
# Wire step 2 of the handshake sends ACK, per rfc7231#section-4.
x = 1
```

The good version produces no finding for this rule.

## Match configuration

<details>
<summary><code>anchored_markers</code> (24 entries)</summary>

```text
step 1
step 2
step 3
step 4
step 5
step 6
step 7
step 8
шаг 1
шаг 2
шаг 3
шаг 4
шаг 5
шаг 6
шаг 7
шаг 8
этап 1
этап 2
этап 3
этап 4
этап 5
этап 6
этап 7
этап 8
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
comment-steps = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/comment-steps -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
