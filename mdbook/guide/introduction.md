# Introduction

agent-slop is a deterministic, AST-based linter for AI-agent comment slop,
with a ratcheting Stop gate for coding-agent sessions.

AI coding agents leave a characteristic residue in source comments: the
change history ("we previously sent a Referer"), the session ("for now",
"as requested"), the review ("nit:", "LGTM", "addressed feedback"), a
restatement of the code below it, a TODO with no condition, a performance
claim with no measurement, a constraint with no source. Each of these rots
the next time the code changes, and each teaches the next agent that writing
them is welcome here.

agent-slop finds that residue and answers it with three surfaces:

- `agent-slop scan` / `agent-slop check` — report findings, in human, JSON,
  JSONL or GitHub workflow-command format.
- `agent-slop hook` — a three-leg hook for coding-agent harnesses: advice on
  every written file (`PostToolUse`), a session baseline (`SessionStart`),
  and a Stop gate that can block the turn when the session *added* slop at
  `error`.
- `agent-slop explain` / `agent-slop rules` — the rule corpus, its levels,
  and the ready-made suppression line per rule.

## Design properties

**Deterministic.** The rules are phrase lists, anchored markers and
structural token-overlap checks over tree-sitter parse trees (via
`ast-grep`). No model decides a verdict. An optional model annotator (the
[Laya judge](./judge.md)) can add context lines, but it can never change a
severity, a fingerprint or an exit code.

**Bilingual.** Most shipped phrase lists cover English and Russian,
including Russian inflection stems (`переименова*`) and a RU→EN glossary so
a Russian comment can be judged against English identifiers.

**Ratcheting.** The Stop gate compares the session's findings against a
multiset pinned at `SessionStart`. Only *added* findings can block; debt
that was already in the tree is reported, never blocking. Baselines are
computed from git, never stored in the repo — there is no baseline file to
merge-conflict or regenerate.

**Fail-open.** No git, no repository, a malformed config tier, an unparsable
rule file, an expired deadline, a crashed judge — the leg degrades and the
exit code stays clean. The binary never exits nonzero on an internal error;
the only deliberate nonzero exit codes are the usage error (`3`) and the
Stop gate's block (`2`).

**Suppression is explicit.** A suppression line needs a reason with a
referent (URL, decision-doc id, issue ref, path, or date). Suppressed
findings are carried in the output, never dropped, so "what did we silence,
and why" stays observable.

## The book

- [Installation](./installation.md) — Nix, home-manager, cargo.
- [Quickstart](./quickstart.md) — the commands that matter, in order.
- [CLI reference](./cli.md) — every subcommand, flag and exit code.
- [Configuration](./configuration.md) — the three config tiers and every key.
- [Rules](./rules/index.md) — one page per rule: what it catches, legitimate
  exceptions, bad/good examples, the suppression line.
- [Suppressions](./suppressions.md) — syntax and the two vacuous-suppression
  meta-rules.
- [Hooks and the ratchet](./hooks-and-the-ratchet.md) — the hook protocol,
  fingerprints, the baseline cache, payload differences between Claude Code
  and grok, and registration recipes.
- [Linter adapters](./linter-adapters.md) — ruff, deadnix, statix, clippy,
  go vet, tsc: cost classes, eligibility, skip reasons.
- [Telemetry](./telemetry.md) — the JSONL row, where it lands, what is never
  recorded.
- [The optional judge](./judge.md) — the Laya second verdict: off by
  default, additive only.
- [Contributing](./contributing.md) — workspace layout, testing, adding a
  rule.
- [FAQ and troubleshooting](./faq.md).
