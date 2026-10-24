# `slop/todo-no-condition`

**Built-in level:** `warning`

## What it catches

A `TODO`, `FIXME`, `XXX` or `HACK` marker with no removal condition. A TODO that cannot be checked is a wish, not debt. A *condition* is any of: an issue ref (`#123`), a decision-doc id (`ADR-064`), a URL, a temporal word (`until`, `before`, `after`, `once`, `when`), a dotted version (`0.16`, `2.0`), or `blocked on`. The marker is matched as a standalone word: `TODO:` and `TODO(owner):` count, `slop/todo-no-condition` and `TODO-family` do not, and markers inside backticks are quoted, not debt.

## Legitimate exception

> A marker that names its removal condition — an issue ref, a version gate, an event ('until the v2 endpoint ships, owner/repo#123'). The most common family in live code, which is exactly why it stays a warning.

## Do instead

Name the condition that makes the TODO closable ('drop when nixpkgs ships >= 0.16') or delete the marker — an unclosable TODO is noise you are asking every future reader to re-triage.

## Example

Bad:

```python
# TODO: fix this later
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/todo-no-condition — TODO with no removal condition — name one or delete it
```

Good:

```python
# TODO: drop when nixpkgs ships >= 0.16
x = 1
```

The good version produces no finding for this rule.

## Match configuration

<details>
<summary><code>markers</code> (4 entries)</summary>

```text
TODO
FIXME
XXX
HACK
```

</details>

<details>
<summary><code>condition_patterns</code> (7 entries)</summary>

```text
#\d+
(?i)(ADR|POL|SPEC|INC|TASK)-\d+
https?://
(?i)\b(until|before|after|once|when)\b
(?i)\b(v?\d+\.\d+)\b
(?i)\bblocked on\b
(?i)\bимя|до |после 
```

</details>

## Configure

```toml
[slop.rules]
todo-no-condition = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/todo-no-condition -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
