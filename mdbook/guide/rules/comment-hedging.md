# `slop/comment-hedging`

**Built-in level:** `warning`

## What it catches

A comment that hedges instead of committing: "should work", "hopefully", "not sure if", "probably fine", "seems to work", "best guess", and Russian ("вроде работает", "не уверен", "должно работать"). The code ships; a comment expressing doubt about shipped code is either a hidden defect (make it a TODO with a condition) or noise (delete it).

## Legitimate exception

> A question addressed to a specific human in a doc-comment on a stub awaiting their answer ('@owner: does the 4096 cap still hold?') — the doubt is the content and it has a recipient.

## Do instead

Either commit or make the doubt actionable: state the fact you verified, or write 'TODO(owner): verify X before Y' so the condition can expire. 'Should work' does neither.

## Example

Bad:

```python
# This should work for most inputs.
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/comment-hedging — hedging 'should work' — doubt about shipped code, not a fact
```

Good:

```python
# TODO(owner): verify the 4096 cap before the public launch.
x = 1
```

The good version produces no finding for this rule.

## Match configuration

- `word_boundaries` = `true`

<details>
<summary><code>phrases</code> (48 entries)</summary>

```text
should work
should be fine
should be okay
should be enough
might not be the best
might not be ideal
may not be the best
may not be ideal
hopefully
not sure if
not sure this
not sure why
not sure about
i'm not sure
im not sure
probably works
probably fine
probably not the best
probably right
best guess
i think this works
this may need
this might need
this could need
seems to work
seems correct
seems right
казаться должно работать
вроде работает
вроде правильно
вроде как работает
наверное работает
наверное правильно
скорее всего работает
должно работать
должно быть достаточно
может быть не лучшим
не уверен
не уверена
не уверен, что
не точно
возможно придётся
возможно придется
возможно понадобится
похоже, что работает
похоже работает
как кажется
как кажется правильным
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
comment-hedging = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/comment-hedging -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
