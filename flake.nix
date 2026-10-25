{
  description = "Rust Cargo workspace with dg decision graph + AI agent governance + quality gates";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-parts.url = "github:hercules-ci/flake-parts";

    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {
    flake-parts,
    self,
    ...
  }:
    flake-parts.lib.mkFlake {inherit inputs;} {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      perSystem = {
        system,
        self',
        ...
      }: let
        pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [inputs.rust-overlay.overlays.default];
        };
        lib = pkgs.lib;
        # Pinned via rust-toolchain.toml (single source of truth).
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        # The same channel without the editor components: the package and the
        # clippy/fmt checks build with the toolchain the repo pins.
        buildToolchain = pkgs.rust-bin.stable.${(lib.importTOML ./rust-toolchain.toml).toolchain.channel}.minimal.override {
          extensions = ["clippy" "rustfmt"];
        };
        rustPlatform = pkgs.makeRustPlatform {
          rustc = buildToolchain;
          cargo = buildToolchain;
        };
        package = pkgs.callPackage ./nix/package.nix {inherit rustPlatform;};

        # Reuse the package's source and vendored dependencies; replace the
        # build with a single cargo invocation.
        cargoCheck = name: script:
          package.overrideAttrs (_: {
            pname = "agent-slop-${name}";
            doCheck = false;
            doInstallCheck = false;
            dontFixup = true;
            buildPhase = ''
              runHook preBuild
              ${script}
              runHook postBuild
            '';
            installPhase = "touch $out";
            postInstall = "";
          });

        hmEval = modules:
          inputs.home-manager.lib.homeManagerConfiguration {
            inherit pkgs;
            modules =
              [
                self.homeManagerModules.default
                {
                  home = {
                    username = "tester";
                    homeDirectory = "/home/tester";
                    stateVersion = "25.05";
                  };
                }
              ]
              ++ modules;
          };
        hmFull = hmEval [
          {
            programs.agent-slop = {
              enable = true;
              mode = "block";
              settings = {
                timeout_ms = 2500;
                rules.comment-paraphrase = "off";
              };
              rules.no-placeholder = goodRule;
              plugins.acme = {
                command = ["/run/current-system/sw/bin/acme-slop" "--strict"];
                timeout_ms = 1500;
                settings.lang = "en";
              };
              hooks = {
                grok.enable = true;
                claude.enable = true;
              };
              telemetry = {
                enable = true;
                root = "/var/lib/agent-slop/telemetry";
                recorder.command = "/run/current-system/sw/bin/recorder";
              };
            };
          }
        ];
        hmMinimal = hmEval [{programs.agent-slop.enable = true;}];

        # A rule that passes `rules lint` and one that must not: the second
        # has a hit fixture but no miss fixture, so the check below fails if
        # the lint step stops catching that.
        goodRule = {
          meta = {
            schema = 1;
            id = "acme/no-placeholder";
            severity = "warning";
          };
          match = {
            kind = "phrase";
            phrases = ["placeholder implementation"];
            word_boundaries = true;
          };
          exception.shape = "A stub a test replaces with a fake on purpose.";
          note.text = "Implement it, or file a TODO that names the condition for removing the stub.";
          fixture = [
            {
              expect = "hit";
              lang = "python";
              provenance = "acme/api src/client.py:88";
              source = "# placeholder implementation, later\nx = 1\n";
            }
            {
              expect = "miss";
              lang = "python";
              provenance = "acme/api src/client.py:120";
              source = "# a real implementation\nx = 1\n";
            }
          ];
        };
        brokenRule = goodRule // {fixture = [(builtins.head goodRule.fixture)];};
        hmBroken = hmEval [
          {
            programs.agent-slop = {
              enable = true;
              rules.no-placeholder = brokenRule;
            };
          }
        ];
      in {
        packages = {
          default = package;
          agent-slop = package;
        };

        apps.default = {
          type = "app";
          program = "${self'.packages.default}/bin/agent-slop";
          meta.description = "Scan source comments for AI-written slop";
        };

        checks = {
          build = package;

          clippy = cargoCheck "clippy" ''
            cargo clippy --offline --workspace --all-targets -- -D warnings
          '';

          fmt = cargoCheck "fmt" ''
            cargo fmt --all --check
          '';

          # Evaluates the module under home-manager's own lib and asserts on
          # the rendered files: config.toml, the grok hook source, and the
          # read-only Claude hook value (empty unless enabled).
          # `agent-slop rules lint` over the rule files the module renders.
          hm-rules-lint = let
            files = hmFull.config.xdg.configFile;
          in
            pkgs.runCommand "agent-slop-hm-rules-lint" {} ''
              export HOME=$TMPDIR
              mkdir -p "$HOME/.config/agent-slop/rules"
              cp ${files."agent-slop/rules/no-placeholder.toml".source} "$HOME/.config/agent-slop/rules/no-placeholder.toml"
              cd "$TMPDIR"
              ${self'.packages.default}/bin/agent-slop rules lint
              ${self'.packages.default}/bin/agent-slop rules test
              touch $out
            '';

          # The same lint on a deliberately broken rule must FAIL: a check
          # that cannot fail proves nothing (POL-026).
          hm-rules-lint-catches-a-broken-rule = let
            files = hmBroken.config.xdg.configFile;
          in
            pkgs.runCommand "agent-slop-hm-rules-lint-broken" {} ''
              export HOME=$TMPDIR
              mkdir -p "$HOME/.config/agent-slop/rules"
              cp ${files."agent-slop/rules/no-placeholder.toml".source} "$HOME/.config/agent-slop/rules/no-placeholder.toml"
              cd "$TMPDIR"
              if ${self'.packages.default}/bin/agent-slop rules lint; then
                echo "rules lint passed a rule with no miss fixture" >&2
                exit 1
              fi
              touch $out
            '';

          hm-module = let
            full = hmFull.config;
            minimal = hmMinimal.config;
            fullJson = pkgs.writeText "hm-full-claude-hooks.json" (builtins.toJSON full.programs.agent-slop.claudeHooks);
            minimalJson = pkgs.writeText "hm-minimal-claude-hooks.json" (builtins.toJSON minimal.programs.agent-slop.claudeHooks);
          in
            pkgs.runCommand "agent-slop-hm-module-check" {nativeBuildInputs = [pkgs.jq pkgs.python3];} ''
              toml=${full.xdg.configFile."agent-slop/config.toml".source}
              python3 - "$toml" <<'PY'
              import sys, tomllib
              d = tomllib.load(open(sys.argv[1], "rb"))["slop"]
              assert d["mode"] == "block", d
              assert d["timeout_ms"] == 2500, d
              assert d["rules"]["comment-paraphrase"] == "off", d
              p = d["plugins"]["acme"]
              assert p["command"] == ["/run/current-system/sw/bin/acme-slop", "--strict"], p
              assert p["timeout_ms"] == 1500 and p["enable"] is True, p
              assert p["settings"] == {"lang": "en"}, p
              PY
              python3 - ${minimal.xdg.configFile."agent-slop/config.toml".source} <<'PY'
              import sys, tomllib
              assert tomllib.load(open(sys.argv[1], "rb")) == {"slop": {"mode": "observe"}}
              PY

              grok=${full.home.file.".grok/hooks/agent-slop.json".source}
              jq -e '.hooks.PostToolUse[0].matcher == "write|search_replace|edit_notebook"' "$grok"
              jq -e '.hooks.SessionStart[0] | has("matcher") | not' "$grok"
              jq -e '.hooks.StopCancelled[0].hooks[0].command == "/run/current-system/sw/bin/recorder"' "$grok"
              cmd=$(jq -r '.hooks.Stop[0].hooks[0].command' "$grok")
              test -x "$cmd"
              grep -q 'AGENT_SLOP_TELEMETRY_ROOT=/var/lib/agent-slop/telemetry' "$cmd"
              grep -q 'bin/agent-slop' "$cmd"

              jq -e '.PostToolUse[0].matcher == "Edit|Write|MultiEdit|NotebookEdit"
                     and .SessionStart[0].matcher == "startup|resume|clear|compact"
                     and (.Stop | length) == 1
                     and (.SessionEnd | length) == 1' ${fullJson}
              jq -e '. == {}' ${minimalJson}
              touch $out
            '';
        };

        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.cargo-nextest
            pkgs.cargo-mutants
            pkgs.cargo-deny
            pkgs.cargo-edit
            pkgs.cargo-watch
            pkgs.cargo-rdme
            pkgs.lefthook
            pkgs.typos
            pkgs.act
            pkgs.actionlint
          ];
          shellHook = ''
            ${pkgs.lefthook}/bin/lefthook install >/dev/null 2>&1 || true
            echo "🦀 Rust $(rustc --version 2>/dev/null || echo '(toolchain)') workspace — dg/agent governance active"
            command -v dg >/dev/null || echo "⚠️  dg not on PATH — decision-graph checks unavailable"
            echo ""
            echo "  Commands:"
            echo "    cargo nextest run --workspace   run all tests"
            echo "    cargo clippy --workspace --all-targets -- -D warnings   lint"
            echo "    cargo fmt --all                 format"
            echo "    cargo deny check                supply-chain audit"
            echo "    cargo rdme --check              verify README is in sync"
            echo "    ./scripts/ci-local.sh           run CI jobs locally via act"
            echo ""
          '';
        };
      };

      flake = {
        homeManagerModules.default = import ./nix/hm-module.nix self;
        homeManagerModules.agent-slop = inputs.self.homeManagerModules.default;
      };
    };
}
