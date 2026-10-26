# Home-manager module for agent-slop, self-contained: no fleet paths, no
# other repository. It owns
#   - ~/.config/agent-slop/config.toml   the user config tier,
#   - ~/.grok/hooks/agent-slop.json      grok's own hook source,
# and exposes the Claude registrations as the read-only option value
# `programs.agent-slop.claudeHooks`; the consumer owns settings.json.
self: {
  lib,
  pkgs,
  config,
  ...
}: let
  cfg = config.programs.agent-slop;
  tomlFormat = pkgs.formats.toml {};
  jsonFormat = pkgs.formats.json {};

  # The wrapper is a PATH carrier around one binary that reads the hook
  # envelope on stdin and decides the leg itself. git is the binary's only
  # subprocess. Fail-open by construction: the binary never exits nonzero on
  # an internal error, so anything outside {0, 2} means it crashed and the
  # harness must be allowed to continue.
  wrapper = pkgs.writeShellApplication {
    name = "agent-slop-hook";
    runtimeInputs = [pkgs.git];
    text = ''
      set +o errexit
      ${lib.optionalString (cfg.telemetry.enable && cfg.telemetry.root != null) ''
        export AGENT_SLOP_TELEMETRY_ROOT=${lib.escapeShellArg cfg.telemetry.root}
      ''}${lib.optionalString (!cfg.telemetry.enable) ''
        # Telemetry off (the default): the binary writes under $HOME/reviews
        # unless told otherwise, so point it at a path no row can land in. The
        # write fails with ENOTDIR and is swallowed, never changing a verdict.
        export AGENT_SLOP_TELEMETRY_ROOT=/dev/null/agent-slop
      ''}bin=${lib.escapeShellArg (lib.getExe cfg.package)}
      [ -x "$bin" ] || exit 0
      "$bin" hook
      rc=$?
      case "$rc" in
        0 | 2) exit "$rc" ;;
        *) exit 0 ;;
      esac
    '';
  };
  command = lib.getExe wrapper;

  # One `[slop.plugins.<name>]` table. The command is an absolute store path
  # (or the caller's explicit argv): only the user tier may declare it, and
  # this module is the user tier.
  pluginEntry = name: p: {
    command =
      if p.command != null
      then p.command
      else if p.package != null
      then
        [
          (lib.getExe' p.package (
            if p.bin != null
            then p.bin
            else name
          ))
        ]
        ++ p.args
      else throw "programs.agent-slop.plugins.${name}: set `package` or `command`";
    inherit (p) enable timeout_ms severity settings env_passthrough;
  };

  # A rule file is only written to the user's rules directory after
  # `agent-slop rules lint` and `rules test` accept it, so a broken rule
  # fails the home build instead of being dropped with a note at runtime.
  lintedRule = name: rule: let
    rendered = tomlFormat.generate "agent-slop-rule-${name}.toml" rule;
    bin = lib.getExe cfg.package;
  in
    pkgs.runCommand "agent-slop-rule-${name}.toml" {} ''
      export HOME="$TMPDIR/home"
      mkdir -p "$HOME"
      ${lib.escapeShellArg bin} rules lint ${rendered}
      ${lib.escapeShellArg bin} rules test ${rendered}
      cp ${rendered} $out
    '';

  leg = timeout: {
    type = "command";
    inherit command timeout;
  };
  recorderLeg = {
    type = "command";
    command = cfg.telemetry.recorder.command;
    timeout = 10;
  };
  recorderOn = cfg.telemetry.enable && cfg.telemetry.recorder.command != null;
  recorderEntries = events:
    lib.genAttrs events (_: [{hooks = [recorderLeg];}]);

  # Claude: advisory PostToolUse on the file just written, SessionStart pins
  # the ratchet baseline, Stop compares against it.
  claudeHooks =
    {
      PostToolUse = [
        {
          matcher = "Edit|Write|MultiEdit|NotebookEdit";
          hooks = [(leg 10)];
        }
      ];
      SessionStart = [
        {
          matcher = "startup|resume|clear|compact";
          hooks = [(leg 15)];
        }
      ];
      Stop = [{hooks = [(leg 15)];}];
    }
    // lib.optionalAttrs recorderOn (recorderEntries cfg.telemetry.recorder.claudeEvents);

  # grok registers the same legs in its own hook source. Its write tools are
  # named differently, and SessionStart carries NO matcher: grok emits the
  # start source "new", which none of Claude's matcher values match, while an
  # omitted matcher matches every source. A matcher on Stop is ignored.
  grokHooks =
    {
      PostToolUse = [
        {
          matcher = "write|search_replace|edit_notebook";
          hooks = [(leg 10)];
        }
      ];
      SessionStart = [{hooks = [(leg 15)];}];
      Stop = [{hooks = [(leg 15)];}];
    }
    // lib.optionalAttrs recorderOn (recorderEntries cfg.telemetry.recorder.grokEvents);
in {
  options.programs.agent-slop = {
    enable = lib.mkEnableOption "agent-slop, a scanner for AI-written slop in source comments";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "agent-slop.packages.\${system}.default";
      description = "The agent-slop package, installed into the profile and carried by the hook wrapper.";
    };

    mode = lib.mkOption {
      type = lib.types.enum ["observe" "block"];
      default = "observe";
      description = ''
        `observe`: the Stop leg records but never exits 2. `block`: an error
        the session ADDED blocks the turn. This is the user tier; a repo's
        `.agent-hooks.toml` can still override it.
      '';
    };

    settings = lib.mkOption {
      type = tomlFormat.type;
      default = {};
      example = lib.literalExpression ''
        {
          timeout_ms = 3000;
          rules.comment-paraphrase = "off";
          exclude.paths = ["vendor/**"];
          judge = { enable = true; url = "http://127.0.0.1:8095/v1/systemone"; };
        }
      '';
      description = ''
        Keys of the `[slop]` section of {file}`~/.config/agent-slop/config.toml`,
        merged over `mode`. Rendered as TOML.
      '';
    };

    rules = lib.mkOption {
      type = lib.types.attrsOf tomlFormat.type;
      default = {};
      example = lib.literalExpression ''
        {
          "no-placeholder" = {
            meta = {
              schema = 1;
              id = "acme/no-placeholder";
              severity = "warning";
            };
            match = {
              kind = "phrase";
              phrases = ["placeholder implementation"];
            };
          };
        }
      '';
      description = ''
        Custom rule files, written to
        {file}`~/.config/agent-slop/rules/<name>.toml`. Keys are file names
        (without the extension); values are the rule documents. Each file is
        run through `agent-slop rules lint` and `agent-slop rules test` when
        the configuration is built, so a rule with a broken fixture, a missing
        miss fixture or a reserved producer fails the build.
      '';
    };

    plugins = lib.mkOption {
      type = lib.types.attrsOf (lib.types.submodule {
        options = {
          package = lib.mkOption {
            type = lib.types.nullOr lib.types.package;
            default = null;
            description = "The plugin package; its bin goes first in `command`.";
          };
          bin = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = "Executable name inside `package` (defaults to the plugin name).";
          };
          command = lib.mkOption {
            type = lib.types.nullOr (lib.types.listOf lib.types.str);
            default = null;
            description = "Full argv override, when the plugin needs more than one executable.";
          };
          args = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [];
            description = "Arguments appended to the executable.";
          };
          enable = lib.mkOption {
            type = lib.types.bool;
            default = true;
          };
          timeout_ms = lib.mkOption {
            type = lib.types.ints.positive;
            default = 2000;
            description = "Wall-clock budget of one call, in milliseconds.";
          };
          severity = lib.mkOption {
            type = lib.types.enum ["error" "warning" "hint" "off"];
            default = "warning";
          };
          settings = lib.mkOption {
            type = tomlFormat.type;
            default = {};
          };
          env_passthrough = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [];
          };
        };
      });
      default = {};
      description = ''
        External plugins, rendered into `[slop.plugins.<name>]` of
        {file}`~/.config/agent-slop/config.toml`. Commands are absolute
        store paths: this module owns the user tier, the only tier allowed
        to declare them.
      '';
    };

    hooks = {
      grok.enable = lib.mkEnableOption "the agent-slop legs in grok's hook source (~/.grok/hooks/agent-slop.json)";

      claude.enable = lib.mkEnableOption "the agent-slop legs in {option}`programs.agent-slop.claudeHooks` (Claude settings.json is not written by this module)";

      command = lib.mkOption {
        type = lib.types.str;
        readOnly = true;
        default = command;
        description = "Path of the fail-open hook wrapper, for wiring other harnesses by hand.";
      };
    };

    claudeHooks = lib.mkOption {
      type = lib.types.attrsOf (lib.types.listOf lib.types.attrs);
      readOnly = true;
      description = ''
        Hook registrations for Claude's `hooks` settings key, keyed by event
        (`PostToolUse`, `SessionStart`, `Stop`, plus the recorder events when
        a recorder is set). Empty unless
        {option}`programs.agent-slop.hooks.claude.enable` is set. Merge these
        into your settings.json wiring, concatenating per-event lists.
      '';
    };

    telemetry = {
      enable = lib.mkEnableOption "scan telemetry: one JSONL row per Stop/scan, appended under the telemetry root";

      root = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/home/me/.local/share/agent-slop/telemetry";
        description = ''
          Telemetry root, exported to the hooks as `AGENT_SLOP_TELEMETRY_ROOT`.
          Null keeps the binary's own default, `$HOME/reviews`. Rows land in
          `<root>/<project>/agent-feedback/<YYYY>/<MM>/<YYYY-MM-DD>.jsonl`.
          The comment text is never recorded.
        '';
      };

      recorder = {
        command = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = ''
            Optional session recorder that owns the same shard (it rewrites a
            session's rows at SessionEnd, which is why the slop row carries a
            carve-out there). When set it is registered next to the slop legs.
          '';
        };

        claudeEvents = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = ["SessionEnd" "PostToolUseFailure" "StopFailure" "PermissionDenied"];
          description = "Claude events the recorder is registered on.";
        };

        grokEvents = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = ["StopCancelled"];
          description = "grok events the recorder is registered on.";
        };
      };
    };
  };

  config = lib.mkMerge [
    {
      programs.agent-slop.claudeHooks =
        lib.optionalAttrs (cfg.enable && cfg.hooks.claude.enable) claudeHooks;
    }
    (lib.mkIf cfg.enable {
      home.packages = [cfg.package];

      xdg.configFile = lib.mkMerge (
        [
          {
            "agent-slop/config.toml".source = tomlFormat.generate "agent-slop-config.toml" {
              slop =
                {inherit (cfg) mode;}
                // cfg.settings
                // lib.optionalAttrs (cfg.plugins != {}) {plugins = lib.mapAttrs pluginEntry cfg.plugins;};
            };
          }
        ]
        ++ lib.mapAttrsToList (name: rule: {
          "agent-slop/rules/${name}.toml".source = lintedRule name rule;
        })
        cfg.rules
      );

      home.file.".grok/hooks/agent-slop.json" = lib.mkIf cfg.hooks.grok.enable {
        source = jsonFormat.generate "agent-slop-grok-hooks.json" {hooks = grokHooks;};
      };
    })
  ];
}
