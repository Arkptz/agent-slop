# Tuning built-ins

The embedded families ship with their phrase lists and thresholds frozen.
`[slop.tune.<family>]` adjusts them per project without forking the corpus:

```toml
[slop.tune.comment-paraphrase]
# A 3-line comment block is an explanation, not a restatement.
max_block_lines = 3

[slop.tune.comment-history]
phrases_extend = ["back then", "in the old version"]

[slop.tune.manual-ddl]
phrases_remove = ["run by hand"]
```

## Merge semantics

- List keys (`phrases_extend`, `anchored_markers_extend`,
  `markers_extend`, `why_markers_extend`, `stopwords_extend`,
  `why_exemption_cancels_extend`) **concatenate** across tiers, lowest
  first — the same direction as `exclude.paths_extend`. An entry is added
  once even if two tiers name it.
- `phrases_remove` concatenates too; each entry must exist in the family's
  phrase list (or in some tier's `phrases_extend`), otherwise the tier
  records a note and the entry is ignored.
- Numeric keys replace per key, highest tier wins, and each is
  range-checked against a per-family allowlist:

| family | key | range |
|---|---|---|
| comment-paraphrase | `min_shared_identifiers` | 1–10 |
| comment-paraphrase | `min_shared_ratio` | 0.0–1.0 |
| comment-paraphrase | `max_block_lines` | 1–50 |
| comment-paraphrase | `min_comment_words` | 1–50 |
| bare-reference | `min_comment_words` | 1–20 |
| docstring-restates-name | `min_head_words` | 1–20 |

There is no wholesale list replacement. The replacement path is a
[custom rule](./custom-rules.md) plus the family set to `off`, which
`self-test` reports as a demotion.

## Failure is a note

An unknown family, an unknown key, a wrong type, an out-of-range value or a
`phrases_remove` entry that matches nothing: each is a *note* on stderr and
that key is ignored. The rest of the table still applies and the command's
exit code does not change:

```console
$ agent-slop rules
config note (User tier): [slop.tune.comment-paraphrase] min_shared_ratio ignored: expected a number in 0..=1
config note (User tier): [slop.tune.nope] ignored: no such rule family
slop/bare-reference          warning
...
```

## The self-test watches the frozen rule

`slop/comment-history` is the one rule that ships at `error`. A tune that
weakens it — `phrases_remove`, or any threshold change — makes
`agent-slop self-test` fail, naming the values, exactly as a demotion
would:

```console
$ agent-slop self-test
self-test: user tier /home/me/.config/agent-slop/config.toml
self-test: repo tier absent (/work/repo/.agent-hooks.toml)
self-test: mode Block from the user tier
self-test: slop/comment-history weakened by tune (phrases_remove (previously)) — the frozen rule no longer matches what it was frozen for
self-test: 19 checks, 1 failed (0 custom rule fixtures run)
```

An `*_extend` on the frozen family is reported as info, not failure: it
only adds matches. A tune on any other family is nobody's business — the
gate was never frozen for them.

## Caching

A tune changes which findings a scan can produce, so it is part of the
ruleset hash that keys the baseline cache: changing a tune value
invalidates cached fingerprints instead of serving stale baselines.
