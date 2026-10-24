# Rules

agent-slop ships sixteen rule families. Each family is one TOML file in
`crates/agent-slop-core/rules/`, compiled into the binary; the pages in this
section document each rule: what it catches, its built-in level, the shape
of the legitimate exception, a bad example and a good one, and the
suppression line.

## How rules are named and levelled

A rule id is `<producer>/<id>`. The slop families are `slop/<family>`;
imported linter findings keep their producer (`ruff/F401`, `deadnix/dead-code`,
`statix/W04`, `clippy/needless_return`, `govet/<analyzer>`, `tsc/TS2322`).

The `severity` in the TOML is the rule's false-positive risk, following
clippy's convention of encoding precision in the level:

- `error` — the measured false-positive rate on the harvested corpus is
  about zero *and* the rule fills its `exception` with the shape of the
  legitimate case. Only `error` findings are in the Stop gate's blocking
  set, and only for a session that *added* them. An imported linter finding
  never reaches this level except by an explicit
  `[slop.rules.overrides]` promotion.
- `warning` — usually right, legitimate exceptions exist. Reported, never
  blocks.
- `hint` — a taste call; reported only when a scan asks for it.
- `off` — the family ships disabled (`slop/comment-language`,
  `slop/rejected-alternative`) and reports nothing until a config tier
  promotes it.

The built-in levels are frozen (SPEC-006); per-tier overrides live in
[Configuration](./configuration.md#slop-rules). `agent-slop rules` prints
the effective table; `agent-slop explain <rule-id>` prints a rule's note,
exception and suppression line.

## How matching works

- **Phrases** (`phrases`) match case-insensitively inside the comment
  payload, as whole words or stems (`переименова*` covers the Russian
  inflections). `word_boundaries = true` enforces the whole-word rule;
  a trailing `*` always makes the phrase a stem.
- **Anchored markers** (`anchored_markers`) must start the comment (after
  the comment marker), optionally as a `LABEL:` prefix — so `nit:` never
  trips inside "digital".
- **Markers** (`markers`) are the TODO family, matched as standalone words
  (`TODO:` and `TODO(owner):` count; `slop/todo-no-condition` does not).
- **Structural rules** do not use phrase lists at all:
  `slop/comment-paraphrase` and `slop/docstring-restates-name` compare the
  comment's tokens against the identifiers of the code that follows it.
- **Code spans are blanked** before matching: a marker inside backticks is
  being quoted, which is what documentation about a rule looks like, not
  debt.
- **Comments are judged as blocks**: a run of adjacent line comments in the
  same column is one paragraph, and block rules (`comment-paraphrase`)
  produce one finding at its first line.

## Languages

Rust, Java, Python (docstrings included), Go, TypeScript, TSX, JavaScript,
Bash, Nix, C, C++ and YAML. The walk matches only the outermost comment
node kinds and never descends into a matched comment — Rust's `doc_comment`
is a child of `line_comment`, and matching both would double-count every
doc comment in the ratchet.

## The pages

Each page carries the rule's id, built-in level, the exception shape
(verbatim from the corpus), bad/good examples verified against the real
binary, and the suppression line `explain` prints.
