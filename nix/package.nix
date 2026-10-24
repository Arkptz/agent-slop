# agent-slop binary. The tests run in the build: the binary is a gate, and a
# gate whose tests do not run is a gate nobody knows the state of. Every
# test builds its own scratch git repository (under /dev/shm or $TMPDIR) and
# points HOME/XDG_CACHE_HOME into it, so nothing reads the builder's git
# config or writes a real telemetry root.
#
# The installed binary is then driven through tests/stop_gate_blocks.sh,
# which must print one line per scenario (a dirty tree blocking, a clean tree
# passing, a grok envelope, an excluded path, an ordinary `NOTE:` heading). A
# build missing any of those lines fails here rather than shipping a gate
# that always allows.
#
# Tests that execute a consumer's feedback recorder are #[ignore]d and need
# AGENT_SLOP_FEEDBACK_RECORD; this package has no such dependency.
{
  lib,
  stdenv,
  rustPlatform,
  git,
}: let
  fs = lib.fileset;
  workspace = (lib.importTOML ../Cargo.toml).workspace.package;
in
  rustPlatform.buildRustPackage {
    pname = "agent-slop";
    inherit (workspace) version;

    src = fs.toSource {
      root = ../.;
      fileset = fs.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../README.md
        ../clippy.toml
        ../rustfmt.toml
        ../crates
      ];
    };

    cargoLock.lockFile = ../Cargo.lock;

    cargoBuildFlags = ["--package" "agent-slop"];
    cargoTestFlags = ["--workspace"];

    # The telemetry fixtures live in /dev/shm (/tmp is an ephemeral realm for
    # project attribution), which darwin does not have.
    doCheck = stdenv.hostPlatform.isLinux;

    # git: every ratchet test drives a real repository.
    nativeCheckInputs = [git];

    preCheck = ''
      export HOME="$TMPDIR/home"
      mkdir -p "$HOME"
    '';

    doInstallCheck = stdenv.hostPlatform.isLinux;

    nativeInstallCheckInputs = [git];

    # Invoked via bash: the sandbox has no /usr/bin/env for the shebang.
    installCheckPhase = ''
      runHook preInstallCheck
      bash crates/agent-slop/tests/stop_gate_blocks.sh "$out/bin/agent-slop" >"$TMPDIR/gate.log" 2>&1 || {
        cat "$TMPDIR/gate.log" >&2
        exit 1
      }
      # Each line is asserted by name: a scenario dropped from the script
      # would otherwise leave the build green with fewer checks.
      for line in \
        'gate: blocked as expected' \
        'gate: clean' \
        'gate: grok envelope blocks' \
        'gate: exclusion honoured' \
        'gate: benign labels allowed'; do
        grep -qF "$line" "$TMPDIR/gate.log" \
          || { echo "missing gate evidence: $line" >&2; cat "$TMPDIR/gate.log" >&2; exit 1; }
      done
      cat "$TMPDIR/gate.log"
      runHook postInstallCheck
    '';

    # Ship the gate proof next to the binary so it stays runnable on a
    # machine without the repo checkout.
    postInstall = ''
      install -Dm755 crates/agent-slop/tests/stop_gate_blocks.sh \
        $out/share/agent-slop/stop_gate_blocks.sh
    '';

    meta = {
      description = "Deterministic linter for AI-agent comment slop with hook ratchets";
      license = with lib.licenses; [mit asl20];
      mainProgram = "agent-slop";
      platforms = lib.platforms.unix;
    };
  }
