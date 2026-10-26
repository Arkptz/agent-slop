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
severity = "warning"     # level of every rule of the plugin; omit it to
                         # let each rule's manifest default_level stand. An
                         # error still arrives as warning unless
                         # [slop.rules.overrides] promotes the rule
settings = { lang = "en" }          # opaque, passed through in the request
env_passthrough = ["VAGUE_WHY_HOME"] # plus PATH, HOME, LANG, LC_ALL, TMPDIR,
                                     # XDG_CACHE_HOME; everything else is scrubbed
```

The name must match `[a-z][a-z0-9-]{1,31}` (a lowercase letter first, 2 to 32
characters) and must not be a reserved producer
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
| `describe-failed` | `describe` exited zero but printed invalid JSON (a nonzero exit is `failed:<exit>`, as for `check`) |
| `protocol-mismatch` | manifest or response names another `protocol` |
| `name-mismatch` | the manifest's `name` differs from the configured key |
| `cost` | class D without an explicit `--plugins` name, or not class A in a hook |
| `timeout` | the call exceeded its budget; the plugin's whole process group is killed, helpers included |
| `output-too-large` | stdout past 8 MiB |
| `failed:<exit>` | `describe` or `check` exited nonzero; `failed:signal` when it was killed by a signal |
| `bad-response` | stdout is not a valid `Response` |
| `deadline` | less than 50 ms left on the caller's deadline, or the deadline ran out while the host was folding the response in (the findings read so far are kept) |
| `no-targets` | no requested file matches the manifest's langs/exts |
| `baseline-<reason>` | under `scan --since`, the base-side pass of an attributable plugin did not finish (`<reason>` is one of the above, `incomplete` when it was cut while normalising); its findings are reported but none count as added |

Findings past 5 000 per plugin per call are truncated before the host
reads them, with a skip entry saying so. Every budget is real time: each
call gets `min(timeout_ms, what is left of the command's deadline − 50 ms)`
recomputed when it starts (plan §3.4), so one plugin's spend shortens the
next one's, and a hook leg's 300 ms ceiling covers all of its plugins
together. Under `scan --since` a deterministic comments plugin, which also
runs over the base, may spend at most half of what is left on its head
call; the base-side pass gets the rest. Every other plugin keeps the
whole deadline. A plugin
the host has to kill on a `SIGINT`/`SIGTERM` aimed at itself (terminal
Ctrl-C, a harness killing the hook's process group) dies with its whole
group before the host exits. On Linux a signal the host inherited as
ignored stays ignored; the check reads `/proc/self/status`, so elsewhere
an inherited ignore is not detected. A plugin upgrade is picked up without cache surgery: the
manifest cache key covers every file in the command line, script
included.

## Suppression and the ratchet

`agent-slop: allow <name>/<rule> -- <reason>` suppresses a plugin finding
exactly like a slop one (plugins have no native mechanism). Under
`scan --since`, a `deterministic` comments plugin also runs over the
changed files as they were at the base, so debt the file already carried
is not charged to the session. When that base-side pass does not finish,
the plugin's findings are reported but none count as added; file-input or nondeterministic findings are
reported but never counted as added — unattributable debt never blocks.
