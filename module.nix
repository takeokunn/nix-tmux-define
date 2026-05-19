# Home Manager module for nix-tmux-define.
#
# Typical usage in a flake-based Home Manager config:
#
#   imports = [ nix-tmux-define.homeManagerModules.default ];
#
#   programs.nix-tmux-define = {
#     enable = true;
#     sessions.myproject = {
#       name    = "myproject";
#       root    = "~/src/myproject";
#       windows = [{
#         name   = "main";
#         layout = {
#           type      = "split";
#           direction = "horizontal";
#           ratio     = 0.6;
#           first     = { type = "pane"; command = "nvim ."; focus = true; };
#           second    = { type = "pane"; command = "cargo watch -x check"; };
#         };
#       }];
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

  # ── Type definitions mirroring the Rust JSON schema ────────────────────────

  envVarType = lib.types.submodule {
    options = {
      key = lib.mkOption { type = lib.types.str; };
      value = lib.mkOption { type = lib.types.str; };
    };
  };

  paneType = lib.types.submodule {
    options = {
      type = lib.mkOption {
        type = lib.types.enum [ "pane" ];
        default = "pane";
      };
      command = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Shell command sent to this pane on startup";
      };
      focus = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Move focus to this pane after session creation";
      };
      title = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Pane title set via select-pane -T";
      };
    };
  };

  # Split node uses lib.types.anything for the recursive children
  # to avoid Nix's infinite-recursion limitation on self-referential types.
  splitType = lib.types.submodule {
    options = {
      type = lib.mkOption {
        type = lib.types.enum [ "split" ];
        default = "split";
      };
      direction = lib.mkOption {
        type = lib.types.enum [ "horizontal" "vertical" ];
        description = "horizontal = side-by-side; vertical = top/bottom";
      };
      ratio = lib.mkOption {
        type = lib.types.float;
        default = 0.5;
        description = "Fraction [0.0, 1.0] of space given to the first child";
      };
      first = lib.mkOption {
        type = lib.types.anything;
        description = "Left / top layout child";
      };
      second = lib.mkOption {
        type = lib.types.anything;
        description = "Right / bottom layout child";
      };
    };
  };

  windowType = lib.types.submodule {
    options = {
      name = lib.mkOption {
        type = lib.types.str;
        description = "tmux window name";
      };
      layout = lib.mkOption {
        type = lib.types.anything;
        description = "Root layout node (pane or recursive split tree)";
      };
      root = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Working directory for this window; overrides session root";
      };
      env = lib.mkOption {
        type = lib.types.listOf envVarType;
        default = [ ];
        description = "Environment variables exported before this window is created";
      };
    };
  };

  sessionType = lib.types.submodule {
    options = {
      name = lib.mkOption {
        type = lib.types.str;
        description = "tmux session name";
      };
      root = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Default working directory for all panes";
      };
      windows = lib.mkOption {
        type = lib.types.listOf windowType;
        description = "Ordered list of windows";
      };
      env = lib.mkOption {
        type = lib.types.listOf envVarType;
        default = [ ];
        description = "Session-level environment variables";
      };
      pre_hook = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Shell command run before the session is created (e.g. nix build)";
      };
    };
  };

  # ── Per-session derivation ────────────────────────────────────────────────

  makeSession =
    sessionCfg:
    let
      jsonFile = pkgs.writeText "nix-tmux-define-${sessionCfg.name}.json" (
        builtins.toJSON sessionCfg
      );
    in
    pkgs.writeShellScriptBin "tmux-session-${sessionCfg.name}" ''
      exec bash <(${cliPkg}/bin/nix-tmux-define print --config ${jsonFile}) "$@"
    '';

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
        Attribute set of tmux sessions.  For each entry a
        `tmux-session-<name>` command is added to your PATH.
      '';
      example = lib.literalExpression ''
        {
          dev = {
            name    = "dev";
            root    = "~/src/myproject";
            windows = [{
              name   = "main";
              layout = {
                type      = "split";
                direction = "horizontal";
                ratio     = 0.6;
                first  = { type = "pane"; command = "nvim ."; focus = true; };
                second = { type = "pane"; command = "cargo watch -x check"; };
              };
            }];
          };
        }
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages =
      [ cliPkg ]
      ++ (lib.mapAttrsToList (_: sessionCfg: makeSession sessionCfg) cfg.sessions);
  };
}
