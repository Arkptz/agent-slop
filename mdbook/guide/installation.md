# Installation

agent-slop ships as one binary, `agent-slop`. Pick whichever route fits your
setup; all three produce the same binary.

## Requirements

- **git** on `PATH`. It is the binary's only subprocess besides the optional
  [linter adapters](./linter-adapters.md). Without git, `scan` and `check`
  still work but the ratchet is off: `--since` has no effect and the Stop
  gate exits 0 with `agent-slop: not a git repository, ratchet off`.
- Rust 1.95 or newer (`rust-version` in the workspace `Cargo.toml`) if you
  build with cargo. The Nix route pins the toolchain for you through
  `rust-toolchain.toml`.

## Nix flake

The flake (`flake.nix`) exposes, for `x86_64-linux`, `aarch64-linux`,
`aarch64-darwin` and `x86_64-darwin`:

| Output | What it is |
|---|---|
| `packages.<system>.default` (alias `packages.<system>.agent-slop`) | the `agent-slop` binary |
| `apps.<system>.default` | `nix run` entry point for the binary |
| `homeManagerModules.default` (alias `homeManagerModules.agent-slop`) | the home-manager module |
| `checks.<system>.{build,clippy,fmt,hm-module}` | the build with tests, clippy `-D warnings`, `cargo fmt --check`, and an evaluated assertion of the home-manager module's rendered files |
| `devShells.<system>.default` | the development shell |

Run it without installing:

```sh
nix run github:Arkptz/agent-slop -- scan
nix run github:Arkptz/agent-slop -- rules
```

Build it:

```sh
nix build github:Arkptz/agent-slop#default
./result/bin/agent-slop --help
```

The package runs the whole workspace test-suite during the build
(`cargoTestFlags = ["--workspace"]`) and then, as an install check, drives
the installed binary through `crates/agent-slop/tests/stop_gate_blocks.sh`.
That script must print five named evidence lines — `gate: blocked as
expected`, `gate: clean`, `gate: grok envelope blocks`, `gate: exclusion
honoured`, `gate: benign labels allowed` — or the build fails. A binary
whose Stop gate cannot block does not get installed. Test execution is
Linux-only (`doCheck = stdenv.hostPlatform.isLinux`). The script is also
shipped at `$out/share/agent-slop/stop_gate_blocks.sh`.

To add the package to a profile or a system flake, add the input and use its
package:

```nix
{
  inputs.agent-slop.url = "github:Arkptz/agent-slop";
  # ...
  # environment.systemPackages = [ inputs.agent-slop.packages.${system}.default ];
}
```

## Home-manager module

Import the module from the flake and enable it:

```nix
{ inputs, ... }: {
  imports = [ inputs.agent-slop.homeManagerModules.default ];

  programs.agent-slop = {
    enable = true;
    mode = "block";            # the Stop gate may exit 2
    hooks.grok.enable = true;  # writes ~/.grok/hooks/agent-slop.json
    hooks.claude.enable = true; # exposes programs.agent-slop.claudeHooks
  };
}
```

The module is self-contained: it references no fleet paths and no other
repository. With `enable = true` it does these things:

- adds `programs.agent-slop.package` to `home.packages`;
- writes `~/.config/agent-slop/config.toml` — the *user tier* of the
  [configuration](./configuration.md) — rendered as TOML from `mode` and
  `settings` under a `[slop]` table;
- with `hooks.grok.enable`, writes `~/.grok/hooks/agent-slop.json`, grok's own
  hook source;
- always (even when `enable = false`) defines the read-only option
  `programs.agent-slop.claudeHooks`; it is empty unless both `enable` and
  `hooks.claude.enable` are set. The module does **not** write Claude's
  `settings.json` — you own that file and merge the value in.

### Option reference

Every option below lives under `programs.agent-slop`. Types and defaults are
copied from `nix/hm-module.nix`.

#### `enable`

- Type: boolean (`mkEnableOption`)
- Default: `false`
- Enables agent-slop, a scanner for AI-written slop in source comments.

#### `package`

- Type: package
- Default: `self.packages.${pkgs.stdenv.hostPlatform.system}.default`
- The agent-slop package, installed into the profile and carried by the hook
  wrapper.

#### `mode`

- Type: enum `"observe"` or `"block"`
- Default: `"observe"`
- `observe`: the Stop leg records but never exits 2. `block`: an error the
  session *added* blocks the turn. This is the user tier; a repository's
  `.agent-hooks.toml` can still override it.

#### `settings`

- Type: TOML attribute set (`pkgs.formats.toml {}`)
- Default: `{}`
- Keys of the `[slop]` section of `~/.config/agent-slop/config.toml`, merged
  over `mode`. Any key documented in [Configuration](./configuration.md) is
  valid. Example:

  ```nix
  settings = {
    timeout_ms = 3000;
    rules.comment-paraphrase = "off";
    exclude.paths = [ "vendor/**" ];
    judge = { enable = true; url = "http://127.0.0.1:8095/v1/systemone"; };
  };
  ```

  The rendered file is `[slop]` containing `mode` plus everything in
  `settings`. With nothing set it is exactly `[slop]\nmode = "observe"`.

#### `hooks.grok.enable`

- Type: boolean (`mkEnableOption`)
- Default: `false`
- Writes the agent-slop legs into grok's hook source,
  `~/.grok/hooks/agent-slop.json`.

#### `hooks.claude.enable`

- Type: boolean (`mkEnableOption`)
- Default: `false`
- Populates `programs.agent-slop.claudeHooks`. Claude's `settings.json` is
  not written by this module.

#### `hooks.command`

- Type: string, read-only
- Default: the store path of the fail-open hook wrapper, `agent-slop-hook`
- Use it to wire other harnesses by hand — see
  [Hooks and the ratchet](./hooks-and-the-ratchet.md#other-harnesses).

#### `claudeHooks`

- Type: attribute set of lists of attribute sets, read-only
- Hook registrations for Claude's `hooks` settings key, keyed by event:
  `PostToolUse`, `SessionStart`, `Stop`, plus the recorder events when a
  recorder is set. Empty unless `programs.agent-slop.hooks.claude.enable`
  (and `enable`) are set. Merge the value into your own settings wiring,
  *concatenating per-event lists* so you do not overwrite other hooks.

#### `telemetry.enable`

- Type: boolean (`mkEnableOption`)
- Default: `false`
- Scan telemetry: one JSONL row per Stop/scan, appended under the telemetry
  root. See [Telemetry](./telemetry.md).

#### `telemetry.root`

- Type: null or string
- Default: `null`
- Example: `"/home/me/.local/share/agent-slop/telemetry"`
- Exported to the hooks as `AGENT_SLOP_TELEMETRY_ROOT`. `null` keeps the
  binary's own default, `$HOME/reviews`. Rows land in
  `<root>/<project>/agent-feedback/<YYYY>/<MM>/<YYYY-MM-DD>.jsonl`. The
  comment text is never recorded.

#### `telemetry.recorder.command`

- Type: null or string
- Default: `null`
- An optional session recorder that owns the same shard (it rewrites a
  session's rows at `SessionEnd`, which is why the agent-slop row carries a
  carve-out there). When set — and `telemetry.enable` is true — it is
  registered next to the agent-slop legs, with a 10 s timeout.

#### `telemetry.recorder.claudeEvents`

- Type: list of strings
- Default: `["SessionEnd" "PostToolUseFailure" "StopFailure" "PermissionDenied"]`
- Claude events the recorder is registered on.

#### `telemetry.recorder.grokEvents`

- Type: list of strings
- Default: `["StopCancelled"]`
- grok events the recorder is registered on.

### What the wrapper does

`hooks.command` points at a `writeShellApplication` named `agent-slop-hook`
with `git` as its runtime input. In order, it:

1. disables `errexit` (`set +o errexit`);
2. if `telemetry.enable` and `telemetry.root` are both set, exports
   `AGENT_SLOP_TELEMETRY_ROOT` to that root;
3. if telemetry is **off** (the default), exports
   `AGENT_SLOP_TELEMETRY_ROOT=/dev/null/agent-slop`, a path no row can land
   in — the write fails with `ENOTDIR` and is swallowed, so a hook never
   writes under `$HOME/reviews` unless you asked for it;
4. exits 0 if the package binary is not executable;
5. runs `agent-slop hook` and maps the result: `0` and `2` pass through,
   anything else becomes `0`. A crash cannot block a turn.

### Registered legs

Both harnesses get the same three legs; they differ in matchers because the
harnesses name their write tools differently:

| Event | Claude (`claudeHooks`) | grok (`~/.grok/hooks/agent-slop.json`) | Timeout |
|---|---|---|---|
| `PostToolUse` | matcher `Edit\|Write\|MultiEdit\|NotebookEdit` | matcher `write\|search_replace\|edit_notebook` | 10 s |
| `SessionStart` | matcher `startup\|resume\|clear\|compact` | **no matcher** | 15 s |
| `Stop` | no matcher | no matcher | 15 s |

grok's `SessionStart` carries no matcher on purpose: grok emits the start
source `"new"`, which none of Claude's matcher values match, while an
omitted matcher matches every source. A matcher on `Stop` is ignored by
both.

## Cargo

From a clone of the repository:

```sh
git clone https://github.com/Arkptz/agent-slop
cd agent-slop
cargo build --release -p agent-slop
./target/release/agent-slop --help
cargo install --path crates/agent-slop     # install into ~/.cargo/bin
```

The release profile uses thin LTO, a single codegen unit and strips symbols
(`[profile.release]` in the root `Cargo.toml`). The workspace denies
`unsafe_code` and builds with `clippy::pedantic` warnings on.

The embedded rule corpus is compiled in with `include_str!`; there is no
runtime data directory to install.
