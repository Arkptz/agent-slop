# Configuration

agent-slop resolves its configuration from three tiers, lowest to highest:

1. **Built-in defaults** — compiled into the binary (the frozen set in
   [`builtin_config`], SPEC-006 "The v1 blocking set").
2. **User tier** — `~/.config/agent-slop/config.toml`.
3. **Repo tier** — `.agent-hooks.toml` at the git toplevel of the directory
   the binary runs in.

Resolution deep-merges per key: a key a higher tier sets replaces the lower
tier's value; a key it never mentions keeps the lower tier's value. Lists
(`exclude.paths`, `exclude.scoped`, `select`, `ignore`) are **replaced
whole**, never concatenated — that is how a repo cancels a user-level
default, and an explicit empty list is the documented way to cancel. The
one additive list is `exclude.paths_extend`: every tier's entries are
concatenated, user tier first. There is exactly one config per git toplevel;
there is no per-directory nesting.

Resolution never fails. A malformed, mistyped or unreadable tier is dropped
entirely with the reason recorded as a *note* — printed to stderr by every
command that loads config — and the lower tiers stand. A missing file (or a
missing `$HOME`, which is normal for hooks run under systemd) is the common
case and records nothing.

`agent-slop self-test` names which tier last set each critical value; see
the [CLI reference](./cli.md#self-test).

## The envelope

Both file tiers share the `.agent-hooks.toml` envelope; `[slop]` is one
section among several, and sections belonging to other tools are ignored.
Every key is optional at every tier. Unknown keys are ignored, so a newer
binary can read an older file (and vice versa: an unknown *rule name* in
`[slop.rules]` is kept and reported in a note, not dropped).

A minimal user tier:

```toml
[slop]
mode = "block"
```

A repository tier that tunes around it:

```toml
[slop]
timeout_ms = 1000

[slop.rules]
comment-history = "warning"

[slop.rules.overrides]
"slop/todo-no-condition" = "error"

[slop.exclude]
paths = ["third_party/**"]

[[slop.exclude.scoped]]
paths = ["tests/**"]
rules = ["slop/todo-no-condition"]

[slop.linters.ruff]
select = ["ERA001"]

[slop.judge]
enable = true
url = "http://127.0.0.1:8095/v1/systemone"
```

## `[slop]` keys

| Key | Type | Default | Meaning |
|---|---|---|---|
| `ref` | string | `"auto"` | The base ref the ratchet compares against. `"auto"` resolves to the merge base of `HEAD` and the first of `origin/HEAD`, `main`, `master` that exists (falling back to `HEAD`). An explicit ref resolves to `merge-base(ref, HEAD)`, never the ref itself. |
| `mode` | `"observe"` or `"block"` | `"observe"` | `observe` never exits 2: the Stop gate records `would_block` in telemetry but always allows. `block` makes the Stop gate exit 2 on added errors. |
| `language` | string | unset | Comment language for `slop/comment-language`; the rule stays off while unset (or `""`). Which codes a future matcher accepts is a SPEC-006 matter; `en`, `ru`, `auto` are the spellings the CLI validates. |
| `timeout_ms` | integer | `4000` | Wall-clock budget in milliseconds for a whole scan or hook leg — the judge shares it rather than adding its own. |

## `[slop.rules]`

Bare `family = level` pairs plus one optional `overrides` subtable.

`family` is one of the sixteen v1 rule families (see
[Rules](./rules/index.md)); `level` is one of:

- `"error"` — reported at error; the only level the Stop gate may block on;
- `"warning"` — reported, never blocks;
- `"hint"` — reported only when the scan asks for it;
- `"off"` — the family does not fire at all: no finding, no advice, no gate
  entry.

| Family (built-in level) | Family (built-in level) |
|---|---|
| `comment-history` (`error`) | `manual-ddl` (`warning`) |
| `comment-meta` (`warning`) | `bare-reference` (`warning`) |
| `review-jargon` (`warning`) | `comment-hedging` (`warning`) |
| `comment-paraphrase` (`warning`) | `comment-steps` (`warning`) |
| `todo-no-condition` (`warning`) | `docstring-restates-name` (`warning`) |
| `perf-claim` (`warning`) | `self-justification` (`warning`) |
| `unsourced-constraint` (`warning`) | `config-deploy-narration` (`warning`) |
| `comment-language` (`off`) | `rejected-alternative` (`off`) |

The built-in `error` set is frozen by SPEC-006 (owner decision 2026-09-21,
amended 2026-10-03 by the TASK-019 telemetry harvest): `comment-history`
alone ships at `error`; `comment-meta` and `review-jargon` were demoted to
`warning` after twelve days of telemetry showed no earned block from either.
Changing the shipped set is a spec amendment, and `agent-slop self-test`
fails while the effective set differs from the frozen one.

### `[slop.rules.overrides]`

Full rule ids to levels — the promote/demote list. Keys may be `slop/*` ids
(overriding the family table) or *imported* ids such as `"ruff/ERA001"`.
Overrides are consulted before the family table; this is also the only path
by which an imported linter finding can reach `error`.

```toml
[slop.rules.overrides]
"slop/todo-no-condition" = "error"
"ruff/F401" = "off"
```

## `[slop.exclude]`

| Key | Type | Merge | Meaning |
|---|---|---|---|
| `paths` | list of globs | highest tier wins whole | Paths skipped everywhere, reporting and Stop gate alike. |
| `paths_extend` | list of globs | concatenated across tiers, user first | Additive exclusions a repo adds to a user's list. |
| `scoped` | list of `{paths, rules}` tables | highest tier wins whole | Path/rule pairs exempting those paths from those rules alone. |

Globs have exactly two documented shapes; anything else is compared
literally:

- `vendor/**` — the directory subtree;
- `**/*.generated.*` — a file-name pattern at any depth.

```toml
[slop.exclude]
paths = ["vendor/**", "**/*.generated.*"]

[[slop.exclude.scoped]]
paths = ["tests/**"]
rules = ["slop/todo-no-condition"]
```

The exclusion predicates are applied by *both* surfaces — `scan`/`check`
reporting and the Stop gate's blocking set — so an excluded path can never
block a turn. Excluded paths appear in the JSON envelope's `skipped[]`
array with reason `"excluded"`, never silently.

## `[slop.linters.<name>]`

Per-adapter settings. An adapter is eligible whether or not a section
exists; `enable = false` is the only way a section turns one off, and the
usual eligibility rules still apply. `<name>` is an adapter name from
[Linter adapters](./linter-adapters.md): `ruff`, `deadnix`, `statix`,
`clippy`, `govet`, `tsc`.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `enable` | boolean | `true` | `false` disables the adapter everywhere. |
| `select` | list of code prefixes | unset | Keep only these codes. Unset means "the linter's own config decides" — the flag is never passed on the command line (ruff's `--select` would *replace* the project's selection), the filter is applied after parsing. |
| `ignore` | list of code prefixes | unset | Drop these codes. Same "after parsing" rule; prefix matching, so `F` selects the whole `F401`-style family. |
| `severity` | level | `"warning"` | The level imported findings report at. Imported findings are *capped*: a linter's own severity can never make a finding `error` — the only path to `error` is an explicit `overrides` promotion. |
| `overrides` | map of full id to level | `{}` | Per-code promote/demote, consulted before `severity` (`"ruff/E501" = "off"`). |

## `[slop.custom]` and `[slop.tune.<family>]`

`[slop.custom]` carries the one switch of the custom-rule layer:

```toml
[slop.custom]
enable = false   # default true; per-key merged like everything else
```

`[slop.tune.<family>]` adjusts an embedded family; see
[Tuning built-ins](./tuning.md). List keys concatenate across tiers,
numeric keys replace per key, and everything invalid is a note, never an
error.

## `[slop.judge]`

The optional [Laya judge](./judge.md) — a second verdict printed as context
next to a finding. Every field defaults to a value that annotates nothing:
the judge is off until a tier switches it on, because the endpoint is a
local model this machine may or may not be running.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `enable` | boolean | `false` | Master switch. |
| `url` | string | `"http://127.0.0.1:8095/v1/systemone"` | laya.cpp's Jev-compatible endpoint. Use the *gateway* port, not the model port: the gateway stays up when the model is stopped and answers 503, which is the failure the judge is built to absorb. Plain `http://` loopback only — the judge never leaves the machine. |
| `timeout_ms` | integer | `2500` | Wall-clock budget for the whole judge leg, not per request. The judge shares its caller's budget; a slow endpoint costs the annotation, never the gate. |
| `max_verdicts` | integer | `4` | Requests per leg. A file with 200 comments must not become 200 round trips inside one tool call. `--max-verdicts 0` on the CLI overrides this for an audit run. |
| `min_confidence` | float | `0.9` | A three-way verdict (`delete`/`rewrite`/`keep`) prints only above this probability. High on purpose: the checkpoint scores 0.628 against a 0.664 always-keep baseline and inherits a length confound, so a low floor would print confidently wrong annotations. |
| `needs_min_p` | float | `0.9` | The same floor for `needs_comment` answers; separate because the two questions are calibrated apart. |

## Severity resolution

For a full rule id, in order:

1. `[slop.rules.overrides]` (and per-linter `overrides` for imported ids) —
   first hit wins;
2. the `[slop.rules]` family table, for `slop/*` ids;
3. the linter's `severity` for imported ids (capped below `error`);
4. otherwise the rule is off or unoverridden: no severity, no finding.

`off` is total silence — a rule at `off` produces no finding, no advice
line, and no gate entry.

## The home-manager route

`programs.agent-slop.mode` and `programs.agent-slop.settings` render the
user tier file; the module merges them under one `[slop]` table. See the
[option reference](./installation.md#option-reference).
