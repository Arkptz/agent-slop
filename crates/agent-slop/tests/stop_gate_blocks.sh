#!/usr/bin/env bash
# TASK-020 / POL-026 acceptance: the Stop gate must be able to fail. Drives
# the built binary's real `hook` leg against a scratch git repository and
# asserts BOTH directions: exit 2 on a dirty tree under mode = "block",
# exit 0 on a clean one. The built-in mode is observe, which never exits 2,
# so each scratch repo carries its own .agent-hooks.toml.
#
# Beyond those two directions it covers the defect classes a round of
# review found, each of which left the gate green here while the binary
# was broken: the grok envelope (both key spellings in one object, with
# snake_case event values), a path the project excluded, and an ordinary
# `NOTE:` heading that is not a review finding. A linter leg is out of
# scope — the hook legs run rules only.
#
# Run: tests/stop_gate_blocks.sh [path/to/agent-slop]
# (default target/debug/agent-slop, i.e. right after `cargo build`).
# Success prints `gate: blocked as expected` and `gate: clean`; any miss
# prints the real exit code and output, and the script exits 1.
set -euo pipefail

BIN="${1:-target/debug/agent-slop}"
if [ ! -x "$BIN" ]; then
  echo "gate: FAIL binary not executable: $BIN" >&2
  exit 1
fi
# The hook runs with cwd inside the scratch repo; a relative path would
# stop resolving the moment we cd.
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

SCRATCH="$(mktemp -d)"
cleanup() { rm -rf -- "$SCRATCH"; }
trap cleanup EXIT

# Hermetic: no machine git config, no user agent-slop tier, no real cache —
# session pins land under $XDG_CACHE_HOME.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export HOME="$SCRATCH/home" XDG_CACHE_HOME="$SCRATCH/cache"
mkdir -p "$HOME"

# Fires slop/comment-history at error (same fixture as src/ratchet.rs tests).
SLOP='# We previously always sent a Referer — that was a bug.'
CLEAN='referer = None'

hook_rc() { # hook_rc <dir> <json> — echoes exit code from inside <dir>
  local repo="$1" payload="$2" rc=0
  printf '%s' "$payload" | (cd "$repo" && exec "$BIN" hook) \
    >"$SCRATCH/hook.stdout" 2>"$SCRATCH/hook.stderr" || rc=$?
  echo "$rc"
}

# expect_rc <want> <got> <label> <success-line>
expect_rc() {
  if [ "$2" -eq "$1" ]; then
    echo "$4"
  else
    echo "gate: FAIL $3: expected exit $1, got $2" >&2
    sed 's/^/  stdout| /' "$SCRATCH/hook.stdout" >&2
    sed 's/^/  stderr| /' "$SCRATCH/hook.stderr" >&2
    return 1
  fi
}

new_repo() { # new_repo <dir> [extra .agent-hooks.toml lines] — one clean commit, mode = "block"
  local dir="$1"
  git init --quiet --initial-branch=main "$dir"
  git -C "$dir" config user.name agent-slop-gate
  git -C "$dir" config user.email gate@example.invalid
  { printf '[slop]\nmode = "block"\n'; [ $# -gt 1 ] && printf '%s\n' "$2"; } \
    >"$dir/.agent-hooks.toml"
  printf '%s\n' "$CLEAN" >"$dir/a.py"
  git -C "$dir" add -A
  git -C "$dir" commit --quiet -m base
}

# Payload builders. The Claude spelling is snake_case keys with PascalCase
# values; grok sends BOTH spellings in ONE object and its camelCase key
# carries a snake_case value (`post_tool_use`), so a binary that dispatches
# on one key, or that declares the pair as a serde alias, silently allows
# every turn.
claude_payload() { # claude_payload <Event> <session-id>
  printf '{"hook_event_name":"%s","session_id":"%s"}' "$1" "$2"
}

grok_payload() { # grok_payload <Event> <snake_event> <session-id>
  printf '{"hookEventName":"%s","sessionId":"%s","hook_event_name":"%s","session_id":"%s","stopHookActive":false}' \
    "$2" "$3" "$1" "$3"
}

FAIL=0

# session_start <repo> <payload> — pins the baseline, must always exit 0
session_start() {
  local rc
  rc="$(hook_rc "$1" "$2")"
  if [ "$rc" -ne 0 ]; then
    echo "gate: FAIL SessionStart expected exit 0, got $rc" >&2
    sed 's/^/  stderr| /' "$SCRATCH/hook.stderr" >&2
    return 1
  fi
}

# ── dirty tree must block (POL-026: a gate that cannot fail is worthless)
BLOCKED="$SCRATCH/blocked"
new_repo "$BLOCKED"
SID="gate-blocked-$$"
session_start "$BLOCKED" "$(claude_payload SessionStart "$SID")" || FAIL=1
printf '%s\n%s\n' "$SLOP" "$CLEAN" >"$BLOCKED/a.py"
rc="$(hook_rc "$BLOCKED" "$(claude_payload Stop "$SID")")"
expect_rc 2 "$rc" "Stop on a dirty tree" "gate: blocked as expected" || FAIL=1

# ── clean tree must pass, under the same mode = "block"
CLEANR="$SCRATCH/clean"
new_repo "$CLEANR"
SID="gate-clean-$$"
session_start "$CLEANR" "$(claude_payload SessionStart "$SID")" || FAIL=1
rc="$(hook_rc "$CLEANR" "$(claude_payload Stop "$SID")")"
expect_rc 0 "$rc" "Stop on a clean tree" "gate: clean" || FAIL=1

# ── the grok envelope: both key spellings in one object, the camelCase key
# carrying grok's own snake_case event value. Parsed as a serde alias this
# is a duplicate field and every leg becomes a no-op at exit 0.
GROKR="$SCRATCH/grok"
new_repo "$GROKR"
SID="gate-grok-$$"
session_start "$GROKR" "$(grok_payload SessionStart session_start "$SID")" || FAIL=1
printf '%s\n%s\n' "$SLOP" "$CLEAN" >"$GROKR/a.py"
rc="$(hook_rc "$GROKR" "$(grok_payload Stop stop "$SID")")"
expect_rc 2 "$rc" "Stop from a grok envelope" "gate: grok envelope blocks" || FAIL=1

# ── an excluded path must not block: the gate reads [slop.exclude], not
# just the reporting path does.
EXCL="$SCRATCH/excluded"
new_repo "$EXCL" '[slop.exclude]
paths = ["vendor/**"]'
mkdir -p "$EXCL/vendor"
SID="gate-excluded-$$"
session_start "$EXCL" "$(claude_payload SessionStart "$SID")" || FAIL=1
printf '%s\n%s\n' "$SLOP" "$CLEAN" >"$EXCL/vendor/a.py"
rc="$(hook_rc "$EXCL" "$(claude_payload Stop "$SID")")"
expect_rc 0 "$rc" "Stop over an excluded path" "gate: exclusion honoured" || FAIL=1

# ── an ordinary heading is not a review finding: slop/review-jargon's
# `label:` marker means a frozen finding id (M-01:, P2-6:), and unbounded
# it also fired on NOTE:, Usage: and the scheme of a bare https: URL.
BENIGN="$SCRATCH/benign"
new_repo "$BENIGN"
SID="gate-benign-$$"
session_start "$BENIGN" "$(claude_payload SessionStart "$SID")" || FAIL=1
{
  printf '# NOTE: the caller closes this socket, see docs/net.md\n'
  printf '# Usage: run with --dry-run first\n'
  printf '%s\n' "$CLEAN"
} >"$BENIGN/a.py"
rc="$(hook_rc "$BENIGN" "$(claude_payload Stop "$SID")")"
expect_rc 0 "$rc" "Stop over ordinary headings" "gate: benign labels allowed" || FAIL=1

exit "$FAIL"
