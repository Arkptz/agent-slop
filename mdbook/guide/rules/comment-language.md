# `slop/comment-language`

**Built-in level:** `off`

## What it catches

Comments not in the configured language. Ships `off`; enable per project by setting `[slop] language` in `.agent-hooks.toml`. The design is script-based detection (Cyrillic codepoints versus ASCII letters) rather than a phrase list, and the language variant stays out of the rule id.

## Legitimate exception

> Identifiers, quoted error text, and proper nouns in the other language are content, not prose. The detector scores whole comments, not individual tokens.

## Do instead

Write the comment in the project's configured language; quoted strings, identifiers and path names in their original language are fine.

## Status

The matcher is reserved: the rule id, its `off` level and its note are shipped, but no script-detection matcher exists in the engine yet. Enabling the family (and setting `language`) currently produces no findings — a Russian comment in an `en` project is not reported. The `--comment-lang` CLI flag and `[slop] language` key are accepted and validated so configuration written today keeps working when the matcher ships.

## Match configuration

This family has no lexical match data; see the status above.

## Configure

```toml
[slop.rules]
comment-language = "warning"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/comment-language -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
