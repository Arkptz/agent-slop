# `slop/self-justification`

**Built-in level:** `warning`

## What it catches

A comment that argues the code is allowed to exist: "import is fine", "not a layer violation", "this is intentional", "allowed here", "RPC boundary", and Russian ("импорт допустим", "так задумано", "обоснование:"). The justification belongs in the review that approved it or in the decision doc; in source it answers a question nobody asked and rots when the boundary moves. Only label and predicate forms are listed — bare adverbs like "deliberately" or "on purpose" are how legitimate engineering prose introduces a *why*, and flagging them produced a flood of false positives.

## Legitimate exception

> A comment that names the constraint a reviewer must verify alongside the permission ('imports worker only through the taskiq RPC proxy — see ADR-021 for the boundary'): the referent and the rule are stated, not just the permission.

## Do instead

State the constraint, not the permission: what the code may NOT do and where the boundary is decided (referent to the doc). If the rule is obvious from the import graph, delete the comment.

## Example

Bad:

```python
# This is intentional — the import is fine here.
import os
```

```console
$ agent-slop check example.py
example.py:1:1 slop/self-justification — self-justification 'import is fine' — argues permission nobody asked for
```

Good:

```python
# Imports worker only through the taskiq RPC proxy; see ADR-021 for the boundary.
import os
```

The good version produces no finding for this rule.

## Match configuration

- `word_boundaries` = `true`

<details>
<summary><code>phrases</code> (33 entries)</summary>

```text
import is allowed
import is fine
import is safe
imports are fine
this import is
the import is
not a layer violation
not a violation
no layer violation
not a leak
this is intentional
this is deliberate
allowed here
permissible here
rpc boundary
rpc-boundary
rpc граница
rpc-граница
граница rpc
архитектурно допустимо
архитектуре допустимо
импорт допустим
импорт разреш
это не протечка
не протечка
не нарушение сло
так задумано
сделано специально
специально так
допустимо, так как
допустимо потому
обоснование:
оправдание
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
self-justification = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/self-justification -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
