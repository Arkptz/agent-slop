# `slop/review-jargon`

**Built-in level:** `warning`

## What it catches

Review vocabulary frozen into source: `nit:`, `LGTM`, `PTAL`, "addressed feedback", "per review", Russian ("исправлено по ревью"), and review-finding ids used as labels (`M-01:`, `P2-6:`, `F5:`). The review is over; its residue addresses a reader who no longer exists. Markers are *anchored* to the start of the comment, so "digital" never trips `nit` and "everything" never trips `LGTM`. The `label:` arm accepts only a short finding-id shape (one to three letters, then digits, five characters at most) followed by fewer than three words; `NOTE:`, `Usage:`, `Safety:` headings and `TODO(...)` are not finding ids. Demoted from `error` to `warning` on 2026-10-03 (TASK-019): the highest firing count of the old `error` rules, with no blocking evidence after the 2026-09 rule-churn days.

## Legitimate exception

> Prose where the token is an ordinary word rather than a review marker — e.g. 'the digital readout' (contains no standalone 'nit:'), or domain text genuinely discussing a review process in third person, not addressing its reader.

## Do instead

Delete the marker; the code and its tests are the answer to the review. If the outcome of a review round is a durable constraint, state the constraint itself with its referent, not the fact that a review happened.

## Example

Bad:

```python
# nit: fixed the loop
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/review-jargon — review marker 'nit:' frozen into the source
```

Good:

```python
# Retry stops after three attempts: the upstream rate-limits at four.
x = 1
```

The good version produces no finding for this rule.

## Match configuration

<details>
<summary><code>anchored_markers</code> (14 entries)</summary>

```text
label:
nit:
nits:
nit.
lgtm
ptal
addressed feedback
addressed review
addressed the feedback
per review
review feedback:
исправлено по ревью
правки по ревью
по замечанию
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
review-jargon = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/review-jargon -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
