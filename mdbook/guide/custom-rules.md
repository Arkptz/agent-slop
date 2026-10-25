# Custom rules

A custom rule is one TOML file with a `<producer>/<id>` name, a match
section and fixtures. Files are read from two directories:

- **User tier**: `~/.config/agent-slop/rules/*.toml` (beside
  `config.toml`) — typically written by Nix.
- **Repo tier**: `<git toplevel>/.agent-slop/rules/*.toml`.

Both directories are non-recursive and sorted. A repo file with the same
`producer/id` as a user file replaces it **whole** — no field merge — so
one rule has one definition; the replacement is announced as a note. A
directory past 256 files, or a file past 256 KiB, drops the excess with a
note.

## A complete rule

```toml
[meta]
schema = 1                       # required; unknown schema drops the rule
id = "acme/no-placeholder"       # <producer>/<id>
severity = "warning"             # warning | hint | off; error is clamped
langs = ["python", "rust"]       # optional; names from the language table
paths = ["src/**"]               # optional include globs
exclude_paths = ["tests/**"]     # optional
targets = ["comment", "docstring"]  # optional; default both

[match]
kind = "phrase"                  # phrase | anchored-marker | marker-without-condition | regex
unit = "comment"                 # comment (default) | block (the comment paragraph)
phrases = ["placeholder implementation", "заглушк*"]
word_boundaries = true
requires_phrases = []            # a second list that must ALSO match
why_markers = []                 # any present → exempt
requires_no_digit = false
requires_no_referent = false
message = "placeholder '{match}' shipped — implement it or file a TODO with a condition"
# kind = "regex": patterns = ['(?i)\bfor now\b']  (case-insensitive by default)
# kind = "marker-without-condition": markers + condition_patterns
# kind = "anchored-marker": anchored_markers

[exception]
shape = "A stub that a test replaces with a fake on purpose, and says so."

[note]
text = "Implement it, or file a TODO that names the condition for removing the stub."

[[fixture]]
expect = "hit"                   # hit | miss
lang = "python"
provenance = "acme/api src/client.py:88"   # required for promotion to error
source = '''
# placeholder implementation, the real retry comes later
def retry(): ...
'''
```

`{match}` in the message template is replaced with the matched text.
Fixtures run through the real engine — `agent-slop rules test`,
`rules lint` and `self-test` all execute them.

## Match kinds

| kind | list it reads | matches |
|---|---|---|
| `phrase` | `phrases` | whole-word (or trailing-`*` stem) phrase, code spans ignored |
| `anchored-marker` | `anchored_markers` | marker at the start of the comment (`wip:`, `label:`) |
| `marker-without-condition` | `markers` + `condition_patterns` | TODO-family marker with no removal condition |
| `regex` | `patterns` | linear-time regex, case-insensitive, code spans blanked |

Regexes are compiled with a 1 MiB size ceiling; a pattern the engine
refuses drops the rule with a note.

## Namespace and reserved producers

The id is `<producer>/<id>`. The producer must match
`^[a-z][a-z0-9-]{0,31}$` and the local id `^[a-z0-9][a-z0-9-]{0,48}$`.
Producers `slop`, `suppression`, `ruff`, `deadnix`, `statix`, `clippy`,
`govet` and `tsc` are reserved — a file claiming one is dropped with a
note, so a custom rule can never be mistaken for an embedded or imported
finding (owner decision 2026-10-25, Q6).

## Severity: declared, clamped, promoted

A custom rule may declare `error`, but it arrives as `warning` with a note.
Promotion to `error` is the standard overrides table, and it is accepted
only when the rule passes the strict bar — a non-empty `[exception]`
shape, a `note` over 40 characters, and at least one `hit` and one `miss`
fixture, each carrying a `provenance` of at least 10 characters:

```toml
[slop.rules.overrides]
"acme/no-placeholder" = "error"
```

A promotion that fails the bar is clamped to `warning` with a note naming
the gap. Either tier may promote (owner decision 2026-10-25, Q1).

## Disabling the layer

```toml
[slop.custom]
enable = false
```

in any tier drops every custom rule; per-key merge applies, so the repo
tier can cancel a user-tier `false` by writing `true`.

## The commands

```console
$ agent-slop rules lint
rules lint: 1 rule files checked, 0 problems

$ agent-slop rules test
ok   acme/no-placeholder fixture 1 (hit)
ok   acme/no-placeholder fixture 2 (miss)
rules test: 2 fixtures, 0 red
```

`agent-slop explain acme/no-placeholder` prints the note, the exception, a
ready suppression line and the defining file. `agent-slop scan` reports
custom findings under their `<producer>/<id>`, and the usual
`agent-slop: allow <producer>/<id> -- <reason>` line suppresses them. A
promoted custom rule gates in the Stop ratchet like any embedded `error`
rule.
