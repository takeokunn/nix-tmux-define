# Home Manager module for nix-tmux-define.
#
# Typical usage in a flake-based Home Manager config:
#
#   imports = [ nix-tmux-define.homeManagerModules.default ];
#
#   programs.nix-tmux-define = {
#     enable = true;
#     sessions.myproject = {
#       configPath = "${config.home.homeDirectory}/.config/nix-tmux-define/myproject.json";
#     };
#   };
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.nix-tmux-define;

  # Prefer user-supplied package; fall back to the one bundled in this flake.
  cliPkg =
    if cfg.package != null then
      cfg.package
    else
      self.packages.${pkgs.stdenv.hostPlatform.system}.default;

  cli = "${cliPkg}/bin/nix-tmux-define";

  validCommandName = value: builtins.match "[A-Za-z0-9][A-Za-z0-9._+-]*" value != null;

  sessionType = lib.types.submodule (
    { name, ... }:
    {
      options = {
        configPath = lib.mkOption {
          type = lib.types.str;
          description = ''
            Absolute runtime path to a JSON, TOML, YAML, or YML session config.

            The module intentionally stores only this path in the Nix store. Do
            not point at a store path unless `allowStoreConfigPath` is enabled.
          '';
          example = lib.literalExpression ''"${config.home.homeDirectory}/.config/nix-tmux-define/dev.json"'';
        };

        commandName = lib.mkOption {
          type = lib.types.str;
          default = "tmux-session-${name}";
          defaultText = lib.literalExpression ''"tmux-session-<attribute-name>"'';
          description = "Command installed into PATH for this session.";
          example = "tmux-dev";
        };

        validateOnActivation = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = "Validate the referenced config during Home Manager activation.";
        };

        allowStoreConfigPath = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = ''
            Permit `configPath` to point into the Nix store.

            This is disabled by default because session configs may contain
            commands, environment variables, and template variables that should
            not be copied into world-readable store paths.
          '';
        };
      };
    }
  );

  # ── Per-session derivation ────────────────────────────────────────────────

  makeSession =
    sessionCfg:
    let
      configArg = lib.escapeShellArg sessionCfg.configPath;
      helpArgs = lib.concatMapStringsSep " " lib.escapeShellArg [
        "Usage: ${sessionCfg.commandName} [--run|--reload|--print|--validate]"
        ""
        "  --run       create or attach to the session (default)"
        "  --reload    replace only the named session"
        "  --print     print the generated bash script"
        "  --validate  validate the session config"
      ];
    in
    pkgs.writeShellScriptBin sessionCfg.commandName ''
      set -euo pipefail

      case "''${1:-}" in
        --reload|-r)
          exec ${cli} reload --config ${configArg}
          ;;
        --print|-p)
          exec ${cli} print --config ${configArg}
          ;;
        --validate|-v)
          exec ${cli} validate --config ${configArg}
          ;;
        --run|"")
          exec ${cli} run --config ${configArg}
          ;;
        --help|-h)
          printf '%s\n' ${helpArgs}
          ;;
        *)
          printf 'unknown option: %s\n' "$1" >&2
          printf '%s\n' ${lib.escapeShellArg "try '${sessionCfg.commandName} --help'"} >&2
          exit 64
          ;;
      esac
    '';

  makeAssertions = sessionName: sessionCfg: [
    {
      assertion = lib.hasPrefix "/" sessionCfg.configPath;
      message = ''
        programs.nix-tmux-define.sessions.${sessionName}.configPath must be an absolute runtime path.
      '';
    }
    {
      assertion =
        sessionCfg.allowStoreConfigPath || !(lib.hasPrefix "${builtins.storeDir}/" sessionCfg.configPath);
      message = ''
        programs.nix-tmux-define.sessions.${sessionName}.configPath points into ${builtins.storeDir}.
        Move the session config outside the Nix store, or set allowStoreConfigPath = true for non-secret configs.
      '';
    }
    {
      assertion = validCommandName sessionCfg.commandName;
      message = ''
        programs.nix-tmux-define.sessions.${sessionName}.commandName must contain only letters, numbers, dots, underscores, plus signs, or hyphens.
      '';
    }
  ];

  sessionsToValidate = lib.filterAttrs (_: sessionCfg: sessionCfg.validateOnActivation) cfg.sessions;

  validationScript = lib.concatStringsSep "\n" (
    lib.mapAttrsToList (
      sessionName: sessionCfg:
      let
        statusLine = lib.escapeShellArg "validating nix-tmux-define session ${sessionName}";
      in
      ''
        printf '%s\n' ${statusLine}
        ${cli} validate --config ${lib.escapeShellArg sessionCfg.configPath} >/dev/null
      ''
    ) sessionsToValidate
  );

in
{
  options.programs.nix-tmux-define = {
    enable = lib.mkEnableOption "nix-tmux-define declarative tmux session manager";

    package = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = ''
        Override the nix-tmux-define package.
        Defaults to the package bundled with this flake.
      '';
    };

    sessions = lib.mkOption {
      type = lib.types.attrsOf sessionType;
      default = { };
      description = ''
        Attribute set of tmux session launchers. Each entry references an
        existing runtime config file and adds a command to PATH.

        Session contents are deliberately not accepted inline here, because
        serializing commands and environment values through Nix would place them
        in the world-readable Nix store.
      '';
      example = lib.literalExpression ''
        {
          dev = {
            configPath = "''${config.home.homeDirectory}/.config/nix-tmux-define/dev.json";
            commandName = "tmux-dev";
          };
        }
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = lib.concatLists (lib.mapAttrsToList makeAssertions cfg.sessions);

    home.activation.nix-tmux-define-validate = lib.mkIf (sessionsToValidate != { }) (
      lib.hm.dag.entryAfter [ "writeBoundary" ] validationScript
    );

    home.packages = [
      cliPkg
    ]
    ++ (lib.mapAttrsToList (_: sessionCfg: makeSession sessionCfg) cfg.sessions);
  };
}
