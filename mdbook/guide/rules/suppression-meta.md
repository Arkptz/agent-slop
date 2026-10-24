# `slop/suppression-empty` and `slop/suppression-vacuous`

**Fixed level:** `error` (when no config entry overrides it)

These two ids are fixed vocabulary, not corpus families: they have no TOML
file, no `explain` entry and no entry in `agent-slop rules`. They are raised
by the suppression parser when an `agent-slop: allow` line is itself
invalid.

| Rule | Raised when |
|---|---|
| `slop/suppression-empty` | the allow line has no `-- <reason>`, or the reason has fewer than 12 non-whitespace characters |
| `slop/suppression-vacuous` | the reason is long enough but carries no referent (URL, decision-doc id, `owner/repo#N`, path with a slash, or `date:YYYY-MM-DD`) |

The finding is reported on the allow line itself. The finding the
suppression was meant to silence stays visible and is marked
`[suppression invalid]`; the Stop gate does not honour it.

```console
$ agent-slop check s2.py s3.py
s2.py:1:1 slop/suppression-empty — invalid suppression: no -- <reason> given — reason must carry a referent (URL, ADR-064, owner/repo#1, path/to, date:2026-09-21)
s2.py:2:1 slop/comment-history — change-history residue 'previously' — narrates the edit, not the code [suppression invalid]
s3.py:1:1 slop/suppression-vacuous — invalid suppression: because I say so okay — reason must carry a referent (URL, ADR-064, owner/repo#1, path/to, date:2026-09-21)
s3.py:2:1 slop/comment-history — change-history residue 'previously' — narrates the edit, not the code [suppression invalid]
```

The way out is to fix the reason. If no config entry names the id, an
unconfigured meta-rule reports at `error`; see
[Suppressions](../suppressions.md) for the referent grammar.
