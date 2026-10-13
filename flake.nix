{
  description = "Rust Cargo workspace with dg decision graph + AI agent governance + quality gates";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs = inputs @ {flake-parts, ...}:
    flake-parts.lib.mkFlake {inherit inputs;} {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      perSystem = {system, ...}: let
        pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [inputs.rust-overlay.overlays.default];
        };
        # Pinned via rust-toolchain.toml (single source of truth).
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in {
        # Eval-only lint check — real lint runs via lefthook / CI.
        checks.fmt-clippy = pkgs.runCommand "fmt-clippy-placeholder" {} ''
          # Actual cargo fmt/clippy runs via lefthook pre-commit and CI.
          # This derivation exists so `nix flake check` evaluates cleanly.
          touch $out
        '';

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
            # pkgs.graphifyy  # codebase knowledge graph (graphify update <path>) — enable when needed
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
    };
}
