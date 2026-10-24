# `slop/comment-history`

**Built-in level:** `error`

## What it catches

Change-history residue: "previously X", "renamed from Y", "used to return Z", "no longer W", and the Russian equivalents ("раньше возвращал", "переименовано из", "теперь вместо"). The history of a line lives in git; a comment that restates it rots the moment the next edit lands and teaches the next agent to write more of it. Dashed diff-residue banners such as `--- original` are covered too. Compound adjectives (`previously-generated`) are not history.

## Legitimate exception

> A migration or compatibility shim whose behaviour DEPENDS on the old state — the comment documents a live data/format conversion, not the edit history. Name the owning decision doc to suppress.

## Do instead

Delete the narration; the history lives in git blame. If the code genuinely branches on the old state, say so in terms of the data ("payloads before v2 lack `kind`"), not of the edit.

## Example

Bad:

```python
# We previously always sent a Referer header here.
referer = None
```

```console
$ agent-slop check example.py
example.py:1:1 slop/comment-history — change-history residue 'previously' — narrates the edit, not the code
```

Good:

```python
# Payloads before v2 lack the kind field, so this shim defaults it (ADR-031).
if payload.get('kind') is None:
    payload['kind'] = 'v1'
```

The good version produces no finding for this rule.

## Match configuration

- `word_boundaries` = `true`

<details>
<summary><code>phrases</code> (52 entries)</summary>

```text
previously
used to be
used to return
used to do
renamed from
renamed to
no longer
was renamed
formerly known as
old name:
больше не
раньше был
раньше была
раньше было
раньше здесь
раньше этот
раньше эта
раньше это
раньше эти
раньше тот
раньше такая
раньше такой
раньше так
ранее здесь
переименова*
раньше возвраща*
раньше возвращал*
раньше вызыва*
раньше использова*
раньше хран*
раньше жил*
раньше лежал*
раньше дела*
раньше шл*
раньше писал*
раньше отдава*
раньше ронял*
раньше падал*
раньше ломал*
раньше срабатыва*
раньше создавал*
раньше существова*
раньше наход*
ранее возвраща*
ранее использова*
теперь вместо
вынесено из
перенесено из
перенесён из
--- original
--- стар
--- было
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
comment-history = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/comment-history -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
