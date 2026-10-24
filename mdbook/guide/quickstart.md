# Quickstart

Sixty seconds from install to a working loop. The examples use the binary
from [Installation](./installation.md); every command also works via
`nix run github:Arkptz/agent-slop -- <args>`.

## 1. See what the rules catch

```console
$ cat demo.py
# We previously always sent a Referer header here.
# TODO: fix this later
referer = None

def get_proxy_user_name():
    """Get the proxy user name."""
    return 1

$ agent-slop check demo.py
demo.py:1:1 slop/comment-history — change-history residue 'previously' — narrates the edit, not the code
demo.py:2:1 slop/todo-no-condition — TODO with no removal condition — name one or delete it
demo.py:6:5 slop/comment-paraphrase — restates the code below (proxy, user, name)
demo.py:6:5 slop/docstring-restates-name — restates the name below (proxy, user, name) — add a fact or delete
```

`check` is advisory: named files, no git needed, always exit 0. The Russian
phrase lists fire the same way — try `# раньше возвращал None` in a Python
comment.

## 2. Scan a repository and read a rule

```console
$ agent-slop scan --linters none --format human
demo.py:1:1 slop/comment-history — change-history residue 'previously' — narrates the edit, not the code
demo.py:2:1 slop/todo-no-condition — TODO with no removal condition — name one or delete it
demo.py:6:5 slop/comment-paraphrase — restates the code below (proxy, user, name)
demo.py:6:5 slop/docstring-restates-name — restates the name below (proxy, user, name) — add a fact or delete

$ agent-slop rules
slop/bare-reference          warning
slop/comment-hedging         warning
slop/comment-history         error
...

$ agent-slop explain slop/comment-history
slop/comment-history: error
  do instead: Delete the narration; the history lives in git blame. ...
  exception: A migration or compatibility shim whose behaviour DEPENDS on the old state ...
  suppress: // agent-slop: allow slop/comment-history -- <reason with a referent: ...>
```

`explain` prints what to do instead, the shape of the legitimate exception,
and the suppression line ready to paste.

## 3. Suppress one finding, with a reason

```python
# agent-slop: allow slop/comment-history -- rollback steps required by ADR-031
# We previously always sent a Referer header here.
referer = None
```

A reason without a referent (URL, `ADR-031`, `owner/repo#1`, a path, or
`date:2026-09-21`) is itself reported, as `slop/suppression-vacuous`. See
[Suppressions](./suppressions.md).

## 4. Ratchet scan against a ref

```sh
agent-slop scan --since main --fail-on error
```

`--since` narrows the scan to files changed since the merge base and
computes the `added` multiset — findings the baseline does not account for.
`--fail-on error` turns a new `error` finding into exit 1. This is the same
ratchet the Stop gate uses, on the command line.

## 5. Wire the Stop gate

Fastest route, home-manager:

```nix
programs.agent-slop = {
  enable = true;
  mode = "block";
  hooks.grok.enable = true;
  hooks.claude.enable = true;
};
```

Then merge `programs.agent-slop.claudeHooks` into your Claude
`settings.json` (the module does not write that file). For hand-rolled
harness wiring, point a hook at `programs.agent-slop.hooks.command` —
see [Hooks and the ratchet](./hooks-and-the-ratchet.md).

Until a repository opts out or tunes, that is the whole setup: defaults are
`mode = "observe"` (record, never block) and the frozen built-in rule
levels. Tune per project in `.agent-hooks.toml` —
[Configuration](./configuration.md).
