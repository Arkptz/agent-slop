# Rule corpus

One TOML file per rule family, covering every language at once. The structural
half — which AST node kinds count as a comment in each grammar — lives in
`src/lang.rs` as a Rust constant, because it must agree with the set of
compiled grammars and a mismatch has to be a compile error. This directory
holds the lexical half: the phrases, the markers, the thresholds, the
exceptions. That is the part patched most often, and the part the miner's
telemetry points at. Phrase and marker matching is always case-insensitive:
the matcher lowercases both sides (`src/rules/lex.rs`).

Matching runs against `ast_grep_core` linked into the binary, not against the
`ast-grep` CLI. The CLI's YAML loader parses a rule against exactly one
grammar, so `kind: line_comment` under `language: python` fails the whole rule
at parse time rather than degrading — measured on ast-grep 0.45.0. With the
kind table in Rust, a rule is parameterised by language at walk time and that
limitation does not apply.

Comment node kinds, measured per grammar rather than assumed. These are the
kinds the walk MATCHES — the outermost node that spans a whole comment:

| language                        | node kinds                     |
|---------------------------------|--------------------------------|
| rust                            | `line_comment`, `block_comment` |
| java                            | `line_comment`, `block_comment` |
| python, go, typescript, tsx, javascript, bash, nix, c, cpp, yaml | `comment` |

Rust's `doc_comment` is deliberately absent: it is a CHILD of the two kinds
above, not a sibling. Measured on ast-grep 0.45.3 against a file holding one
`///`, one `//!`, one `//`, one `/** */` and one `/* */`: `line_comment`
matched 3 nodes (including `///` and `//!`, text spanning from column 0 and
including the marker), `block_comment` matched both block forms, and
`doc_comment` matched 2 nodes whose ranges start at column 3–4 and whose text
is the payload with the marker stripped. Matching all three kinds therefore
yields TWO overlapping findings for every doc comment, with two different
snippets and hence two different fingerprints — the ratchet would count one
comment twice and a suppression written for one of them would not cover the
other. The walk matches only the outer kinds and does not descend into a
matched comment; `doc_comment`, where it exists, is read as the marker-stripped
payload of its parent, never as an independent match.

`severity` is the rule's false-positive risk, following clippy's convention of
encoding precision in the level rather than in prose:

- `error` — the measured false-positive rate on this repo's corpus is about
  zero AND the rule fills `exception` with the shape of the legitimate case.
  Only the ratchet counts these, and only these can block a turn. An imported
  linter finding never reaches this level.
- `warning` — usually right, legitimate exceptions exist. Reported, never
  blocks.
- `hint` — a taste call. Reported only when the scan asks for it.
- `off` — the family ships disabled; it reports nothing until a config tier
  promotes it (`slop/comment-language` and `slop/rejected-alternative`
  ship off).

Every rule's `note` states what to do instead. A finding the agent cannot act
on teaches it to route around the gate.

The full contract — field names, fingerprint formula, suppression syntax — is
SPEC-006. The engine rationale is ADR-064.
