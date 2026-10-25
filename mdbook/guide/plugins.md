# Writing a plugin

A plugin is any executable that answers two subcommands over JSON:

1. `describe` — prints a **manifest** on stdout: what the plugin is called,
   what it reads and which rules it can report.
2. `check` — reads a **request** on stdin (repo root, paths, and for
   comment plugins the extracted comments with positions) and prints a
   **response** on stdout.

One process per call, no daemon. Exit status 0 on `check` means the
response is valid; anything else is a failure recorded as a skip.

The wire types live in the `agent-slop-protocol` crate; the JSON shapes
are the contract. Unknown fields are ignored on read, so a field added in
a minor release does not break an older host; a breaking change bumps the
`protocol` number (the host accepts exactly `1`).

## Declaring it

Only the **user tier** may declare a plugin — the tier a Nix module owns.
A cloned repository may not choose what executes: `[slop.plugins.<name>]`
in `.agent-hooks.toml` may set `enable = false` and lower `severity`, and
everything else is refused with a config note.

```toml
# ~/.config/agent-slop/config.toml
[slop.plugins.vague-why]
command = ["/run/current-system/sw/bin/vague-why-slop"]  # absolute, or PATH-resolved
enable = true
timeout_ms = 2000        # wall clock of one call
severity = "warning"     # cap; an error still arrives as warning unless
                         # [slop.rules.overrides] promotes the rule
settings = { lang = "en" }          # opaque, passed through in the request
env_passthrough = ["VAGUE_WHY_HOME"] # plus PATH, HOME, LANG, LC_ALL, TMPDIR,
                                     # XDG_CACHE_HOME; everything else is scrubbed
```

The name must match `[a-z0-9-]{1,32}` and must not be a reserved producer
(`slop`, `suppression`, `ruff`, `deadnix`, `statix`, `clippy`, `govet`,
`tsc`) or the producer of a loaded custom rule — findings are reported as
`<name>/<rule>`, and the namespace must stay unambiguous.

## A complete plugin

A plugin is a manifest, a check, and nothing else — this one is complete
(python, stdlib only):

```python
#!/usr/bin/env python3
"""A comment plugin: flags comments that claim a need without naming it."""
import json
import re
import sys

MANIFEST = {
    "protocol": 1,
    "name": "vague-why",
    "version": "0.1.0",
    "input": "comments",          # or "files"
    "cost": "A",                  # A < B < C < D; hooks only run cost A
    "langs": [],                  # empty = every language the scanner knows
    "deterministic": True,        # required for --since ratcheting
    "rules": [
        {
            "id": "needed",
            "default_level": "warning",
            "note": "Name the constraint that makes the code necessary.",
            "exception": "The comment names a product, bug or version.",
        }
    ],
}
VAGUE = re.compile(r"\b(needed|required|necessary) (for|because of) (reasons|compatibility)\b", re.I)


def check(request):
    findings = []
    for f in request["files"]:
        for c in f.get("comments", []):
            if VAGUE.search(c["text"]):
                findings.append(
                    {
                        "rule": "needed",
                        "path": f["path"],
                        "comment": c["index"],   # position comes from OUR comment
                        "message": "claims a need without naming the constraint",
                    }
                )
    return {"protocol": 1, "findings": findings, "diagnostics": []}


if __name__ == "__main__":
    if sys.argv[1:] == ["describe"]:
        print(json.dumps(MANIFEST))
    elif sys.argv[1:] == ["check"]:
        print(json.dumps(check(json.load(sys.stdin))))
    else:
        sys.exit(2)
```

`input = "files"` plugins get `path` and `lang` only and report absolute
positions themselves (`line`, `col`, `end_line`, optional `snippet`);
`input = "comments"` plugins point at a comment by index and the host
supplies position and snippet.

## What it looks like

```console
$ agent-slop scan a.py --format human
a.py:1:1 vague-why/needed — claims a need without naming the constraint
skipped deadnix: no-targets
skipped statix: no-targets
```

```console
$ agent-slop check a.py
a.py:1:1 vague-why/needed — claims a need without naming the constraint
```

In a hook leg the same finding reaches the model as advice:

```console
$ agent-slop hook <<< '{"hook_event_name":"PostToolUse","tool_name":"Write",
                        "tool_input":{"file_path":"a.py"},"session_id":"s1"}'
{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"a.py:1 vague-why/needed — claims a need without naming the constraint"}}
```

Exit 0 in every case: plugin findings are advisory in hooks by decision
(owner decision 2026-10-25, Q3), and the Stop gate does not run plugins at
all.

## Failure is a skip

Every failure is recorded under the plugin's name and changes no exit
code:

```console
$ agent-slop scan a.py --format json
{"skipped":[{"reason":"absent","tool":"ghost"}],...}
```

| reason | when |
|---|---|
| `absent` | the executable is not on `PATH`/does not exist |
| `describe-failed` | `describe` exited nonzero or printed invalid JSON |
| `protocol-mismatch` | manifest or response names another `protocol` |
| `name-mismatch` | the manifest's `name` differs from the configured key |
| `cost` | class D without an explicit `--plugins` name, or not class A in a hook |
| `timeout` | the call exceeded its budget; the process is killed |
| `output-too-large` | stdout past 8 MiB |
| `failed:<exit>` | `check` exited nonzero |
| `bad-response` | stdout is not a valid `Response` |
| `deadline` | less than 50 ms left on the caller's deadline |
| `no-targets` | no requested file matches the manifest's langs/exts |

Findings past 5 000 per plugin per call are truncated, with a skip entry
saying so.

## Suppression and the ratchet

`agent-slop: allow <name>/<rule> -- <reason>` suppresses a plugin finding
exactly like a slop one (plugins have no native mechanism). Under
`scan --since`, a `deterministic` comments plugin also runs over the
changed files as they were at the base, so debt the file already carried
is not charged to the session; file-input or nondeterministic findings are
reported but never counted as added — unattributable debt never blocks.
