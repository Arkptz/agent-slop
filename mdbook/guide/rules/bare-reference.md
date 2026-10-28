# `slop/bare-reference`

**Built-in level:** `warning`

## What it catches

A ticket or decision-doc id standing in for the explanation: `(KAN-621)`, "see POL-002". The reader who does not open the tracker learns nothing, and the id breaks when the doc moves. An id is an uppercase prefix of 2 to 10 characters, a dash, and at least two digits. It fires when the comment block has an id and fewer than four words of its own *outside* parentheses. Standards (`UTF-8`, `SHA-256`, `RFC-9110`, `ISO-…`, `CVE-…`, `PEP-…`) and one-digit suffixes (product versions like `GPT-6`) are not ids.

## Legitimate exception

> An id inside a sentence that already states the rule in its own words ('keys are Fernet-encrypted at rest (ADR-003)').

## Do instead

State the rule or decision in your own words and keep the id in parentheses after it: the id links code to doc, the words let the reader skip the doc.

## Example

Bad:

```python
# (KAN-621)
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/bare-reference — 'KAN-621' stands in for the explanation — say the rule in words
```

Good:

```python
# Keys are Fernet-encrypted at rest (ADR-003).
x = 1
```

The good version produces no finding for this rule.

## Match configuration

- `min_comment_words` = `4`

<details>
<summary><code>stopwords</code> (17 entries)</summary>

```text
UTF
SHA
ISO
RFC
CVE
PEP
HTTP
TLS
AES
MD
IPV
X
BT
ECMA
IEEE
WCAG
RS
```

</details>

## Configure

```toml
[slop.rules]
bare-reference = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/bare-reference -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).

## Plan and review markers

`P3-3`, `W2-1` and other one-or-two-character section ids name a plan or
review round that no longer exists once the session ends. They are judged
by the same bar as tracker ids: surrounded by a full sentence the marker is
a citation; standing alone (or in trailing parens with a stub) it is
session residue and fires. Review-finding labels with a colon (`M-01:`)
belong to `slop/review-jargon` instead, and product or metric shapes
(`A4-2` paper, `P99-2` percentile, `D2-4`) are exempt as stopwords.
