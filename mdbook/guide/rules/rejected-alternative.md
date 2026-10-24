# `slop/rejected-alternative`

**Built-in level:** `off`

## What it catches

A comment that defines the code by what it is *not*: "instead of", "rather than", "вместо", "а не", or a negation opening a clause (`, not Y`, `: not Y`, `— не Y`). The rejected Y is not in the code, so the comment describes a state the reader cannot see. Inline code spans are blanked first, so a quoted `X, not Y` is documentation of the shape. Ships `off`: in the harvested corpus about one comment in ten has the shape, many deliberately. A project whose rules forbid it enables the family in `.agent-hooks.toml`.

## Legitimate exception

> The negation carries a distinguishing fact the reader needs ('the discriminator is the key, not an empty data field'), or states a test's contract ('the server refuses rather than tunnelling directly').

## Do instead

State the current behaviour and its reason affirmatively: not 'cut at the cap, not checked after reading: the body comes from a foreign server' but 'reading stops at the cap: the body size comes from a foreign server'.

## Example

The family ships `off`; the output below was produced with `rejected-alternative = "warning"` in `.agent-hooks.toml`.

Bad:

```python
# Use the cached value instead of recomputing it.
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/rejected-alternative — 'instead of' defines the code by an alternative that is not there
```

Good:

```python
# Reads the cached value; recomputing costs a network round trip.
x = 1
```

The good version produces no finding for this rule.

## Match configuration

- `word_boundaries` = `true`

<details>
<summary><code>phrases</code> (5 entries)</summary>

```text
а не
вместо
вместо того чтобы
instead of
rather than
```

</details>

<details>
<summary><code>contrast_negations</code> (2 entries)</summary>

```text
не
not
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
rejected-alternative = "warning"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/rejected-alternative -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
