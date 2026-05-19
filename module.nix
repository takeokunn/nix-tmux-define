# Home Manager module for nix-tmux-define.
# Usage in flake.nix:
#   imports = [ nix-tmux-define.homeManagerModules.default ];
#   programs.nix-tmux-define = {
#     enable = true;
#     sessions.myproject = { name = "myproject"; root = "/src/myproject"; windows = [...]; };
#   };
{ self }:
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.nix-tmux-define;

  # Resolve the CLI package: prefer user override, else use the flake's own build.
  cliPkg = if cfg.package != null then cfg.package else self.packages.${pkgs.stdenv.hostPlatform.system}.default;

  # ── Nix types mirroring the Rust JSON schema ──────────────────────────────

  # Leaf pane type
  paneType = lib.types.submodule {
    options = {
      type = lib.mkOption {
        type = lib.types.enum [ "pane" ];
        default = "pane";
        description = "Node discriminator";
      };
      command = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Shell command sent to this pane on startup";
      };
      focus = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "If true, move focus to this pane after session creation";
      };
    };
  };

  # Split node; uses lib.types.anything for the recursive children so that
  # the Nix type system is not forced into infinite recursion.
  splitType = lib.types.submodule {
    options = {
      type = lib.mkOption {
        type = lib.types.enum [ "split" ];
        default = "split";
        description = "Node discriminator";
      };
      direction = lib.mkOption {
        type = lib.types.enum [ "horizontal" "vertical" ];
        description = "Split direction: horizontal (side-by-side) or vertical (top/bottom)";
      };
      ratio = lib.mkOption {
        type = lib.types.float;
        default = 0.5;
        description = "Fraction of space (0.0–1.0) allocated to the *first* child";
      };
      first = lib.mkOption {
        type = lib.types.anything;
        description = "First (left / top) layout child";
      };
      second = lib.mkOption {
        type = lib.types.anything;
        description = "Second (right / bottom) layout child";
      };
    };
  };

  # Union: accept either a pane or a split
  layoutType = lib.types.oneOf [ paneType splitType ];

  # Single window type
  windowType = lib.types.submodule {
    options = {
      name = lib.mkOption {
        type = lib.types.str;
        description = "tmux window name";
      };
      layout = lib.mkOption {
        type = lib.types.anything;
        description = "Root layout node (pane or split tree)";
      };
      root = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Working directory for this window (overrides session root)";
      };
    };
  };

  # Environment variable pair
  envVarType = lib.types.submodule {
    options = {
      key = lib.mkOption { type = lib.types.str; };
      value = lib.mkOption { type = lib.types.str; };
    };
  };

  # Full session type
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
        description = "Environment variables exported before session creation";
      };
      pre_hook = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Shell command to run before creating the session";
      };
    };
  };

  # ── Per-session derivations ───────────────────────────────────────────────

  makeSession = sessionCfg:
    let
      sessionName = sessionCfg.name;

      # Serialise the session definition into a JSON file placed in the Nix store
      jsonFile = pkgs.writeText "nix-tmux-define-${sessionName}.json"
        (builtins.toJSON sessionCfg);

      # Wrapper script: invoke CLI → pipe to bash
      launchScript = pkgs.writeShellScriptBin "tmux-session-${sessionName}" ''
        exec bash <(${cliPkg}/bin/nix-tmux-define --config ${jsonFile} --print) "$@"
      '';
    in
    launchScript;

in
{
  options.programs.nix-tmux-define = {
    enable = lib.mkEnableOption "nix-tmux-define declarative tmux session manager";

    package = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = ''
        Override the nix-tmux-define package.
        When null (default), the package bundled with this flake is used.
      '';
    };

    sessions = lib.mkOption {
      type = lib.types.attrsOf sessionType;
      default = { };
      description = ''
        Attribute set of tmux session definitions keyed by an arbitrary name.
        Each definition is serialised to JSON and a corresponding
        `tmux-session-<name>` command is added to your PATH.
      '';
      example = lib.literalExpression ''
        {
          dev = {
            name = "dev";
            root = "~/src/myproject";
            windows = [
              {
                name = "main";
                layout = {
                  type = "split";
                  direction = "horizontal";
                  ratio = 0.6;
                  first  = { type = "pane"; command = "nvim ."; focus = true; };
                  second = { type = "pane"; command = "cargo watch -x check"; };
                };
              }
            ];
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
