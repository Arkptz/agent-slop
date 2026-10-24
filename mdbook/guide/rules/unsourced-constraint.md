# `slop/unsourced-constraint`

**Built-in level:** `warning`

## What it catches

An external constraint with no source: "some providers", "older versions", "due to a bug", "a known bug", "known issue", and Russian ("из-за бага", "старые версии"). If you cannot name the provider, the bug or the version, you do not know it is true. A *referent* in the comment — URL, decision-doc id, `owner/repo#N`, a path with a slash, or `date:YYYY-MM-DD` — counts as a source (`requires_no_referent`). A bare `#412` is *not* a referent; `owner/repo#412` is.

## Legitimate exception

> The constraint carries its source: a product+version, an issue URL, an RFC section. 'Some providers reject X (Fastly bug #412, fixed 2026-03)' is a fact; 'some providers reject X' is a rumour.

## Do instead

Name the source — which provider, which bug tracker entry, which version. If you cannot name it, you do not know it is true; delete the comment or mark it as hypothesis with its referent.

## Example

Bad:

```python
# Some providers reject this header.
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/unsourced-constraint — constraint 'some providers' names no source — name the product, bug, or version
```

Good:

```python
# Fastly rejects this header (https://example.com/bugs/412).
x = 1
```

The good version produces no finding for this rule.

## Match configuration

- `requires_no_referent` = `true`

<details>
<summary><code>phrases</code> (20 entries)</summary>

```text
some providers
some servers
some clients
some libraries
some systems
certain providers
some versions
older versions
old versions
due to a bug
because of a bug
a known bug
upstream bug
browser bug
known issue
по некоторым
из-за бага
старые версии
некоторые серверы
некоторые провайдеры
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
unsourced-constraint = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/unsourced-constraint -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
