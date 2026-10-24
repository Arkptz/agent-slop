# `slop/perf-claim`

**Built-in level:** `warning`

## What it catches

A speed or efficiency claim with no number and no measurement method: "faster than", "blazingly fast", "highly optimized", "zero-copy", "O(1)", "cache-friendly", and Russian ("гораздо быстрее"). Marketing copy that survives refactors that invalidate it. The rule is suppressed by *any digit* anywhere in the comment (`requires_no_digit`), so a claim with a number, a benchmark id or a big-O with a variable is treated as carrying evidence.

## Legitimate exception

> A claim carrying its own evidence: a number, a benchmark name, a big-O with the variable, or a comparator naming the alternative ('cheaper than a fresh parse, bench in #123').

## Do instead

Cite the measurement (number, bench, issue) or drop the adjective — an unverifiable perf claim is marketing copy that survives refactors that invalidate it.

## Example

Bad:

```python
# This is much faster than the naive approach.
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/perf-claim — perf claim 'faster than' without a measurement — cite it or drop it
```

Good:

```python
# About 3x faster than a fresh parse; bench in Arkptz/dg#41.
x = 1
```

The good version produces no finding for this rule.

## Match configuration

- `requires_no_digit` = `true`

<details>
<summary><code>phrases</code> (20 entries)</summary>

```text
faster than
much faster
significantly faster
way faster
blazingly fast
highly optimized
heavily optimized
optimal path
zero-copy
constant time
linear time
O(1)
avoids a copy
avoids allocation
no allocations
cache-friendly
performance critical
гораздо быстрее
значительно быстрее
быстрее, чем
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
perf-claim = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/perf-claim -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
