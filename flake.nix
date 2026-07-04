{
  description = "nix-tmux-define — declarative tmux session manager (Nix + Rust hybrid)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-parts,
      ...
    }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      perSystem =
        { pkgs, system, ... }:
        let
          # The release package. buildRustPackage runs `cargo test` in its
          # checkPhase by default, so building this also runs the unit tests.
          pkg = pkgs.rustPlatform.buildRustPackage {
            pname = "nix-tmux-define";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            meta = {
              description = "Declarative tmux session manager driven by JSON / Home Manager";
              homepage = "https://github.com/takeokunn/nix-tmux-define";
              license = pkgs.lib.licenses.mit;
              mainProgram = "nix-tmux-define";
            };
          };

          # Source-tree checks (formatting, workflow linting) only need the file
          # tree, so they run as lightweight runCommand derivations.
          srcCheck =
            name: nativeBuildInputs: script:
            pkgs.runCommand "nix-tmux-define-${name}" { inherit nativeBuildInputs; } ''
              cd ${./.}
              ${script}
              touch $out
            '';

          hmLib = pkgs.lib.extend (
            _final: _prev: {
              hm.dag.entryAfter = _deps: value: value;
            }
          );

          moduleUnderTest = import ./module.nix { inherit self; };

          baseHomeModule =
            { lib, ... }:
            {
              options = {
                home.packages = lib.mkOption {
                  type = lib.types.listOf lib.types.package;
                  default = [ ];
                };
                home.activation = lib.mkOption {
                  type = lib.types.attrsOf lib.types.anything;
                  default = { };
                };
                assertions = lib.mkOption {
                  type = lib.types.listOf (
                    lib.types.submodule {
                      options = {
                        assertion = lib.mkOption { type = lib.types.bool; };
                        message = lib.mkOption { type = lib.types.str; };
                      };
                    }
                  );
                  default = [ ];
                };
              };
            };

          evalHomeConfig =
            sessions:
            (hmLib.evalModules {
              specialArgs = { inherit pkgs; };
              modules = [
                baseHomeModule
                (
                  { config, pkgs, ... }:
                  moduleUnderTest {
                    inherit config pkgs;
                    lib = hmLib;
                  }
                )
                {
                  programs.nix-tmux-define = {
                    enable = true;
                    package = pkg;
                    inherit sessions;
                  };
                }
              ];
            }).config;

          validHomeConfig = evalHomeConfig {
            dev = {
              configPath = "/tmp/dev.json";
              commandName = "tmux-dev";
            };
          };

          storePathConfig = evalHomeConfig {
            secret.configPath = "${pkg}/share/secret.json";
          };

          relativePathConfig = evalHomeConfig {
            bad.configPath = "relative.json";
          };

          invalidCommandConfig = evalHomeConfig {
            bad = {
              configPath = "/tmp/dev.json";
              commandName = "-bad";
            };
          };

          homeManagerModuleCheck =
            let
              hasValidation =
                pkgs.lib.hasInfix "validate --config /tmp/dev.json"
                  validHomeConfig.home.activation."nix-tmux-define-validate";
              allValidAssertionsPass = pkgs.lib.all (a: a.assertion) validHomeConfig.assertions;
              hasLauncher = pkgs.lib.any (pkg: pkg.name == "tmux-dev") validHomeConfig.home.packages;
              rejectsStorePath = pkgs.lib.any (
                a: !a.assertion && builtins.match ".*points into.*" a.message != null
              ) storePathConfig.assertions;
              rejectsRelativePath = pkgs.lib.any (
                a: !a.assertion && builtins.match ".*absolute runtime path.*" a.message != null
              ) relativePathConfig.assertions;
              rejectsInvalidCommand = pkgs.lib.any (
                a: !a.assertion && builtins.match ".*commandName.*" a.message != null
              ) invalidCommandConfig.assertions;
            in
            if
              allValidAssertionsPass
              && hasLauncher
              && hasValidation
              && rejectsStorePath
              && rejectsRelativePath
              && rejectsInvalidCommand
            then
              srcCheck "home-manager-module" [ ] ""
            else
              throw ''
                Home Manager module evaluation check failed:
                  allValidAssertionsPass=${toString allValidAssertionsPass}
                  hasLauncher=${toString hasLauncher}
                  hasValidation=${toString hasValidation}
                  rejectsStorePath=${toString rejectsStorePath}
                  rejectsRelativePath=${toString rejectsRelativePath}
                  rejectsInvalidCommand=${toString rejectsInvalidCommand}
              '';
        in
        {
          # ── Packages ────────────────────────────────────────────────────────
          packages.default = pkg;
          packages.nix-tmux-define = pkg;

          # ── App (nix run) ────────────────────────────────────────────────────
          apps.default = {
            type = "app";
            program = "${pkg}/bin/nix-tmux-define";
            meta.description = "Run the nix-tmux-define CLI";
          };

          # ── Dev Shell ─────────────────────────────────────────────────────────
          devShells.default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rust-analyzer
              clippy
              rustfmt
              tmux
              nixd
              nixfmt
              actionlint
            ];
            # Plain `echo` lines (not a heredoc) so nixfmt's string
            # re-indentation can never break the banner.
            shellHook = ''
              echo
              echo "=== nix-tmux-define Development Shell ==="
              echo
              echo "Build & run:"
              echo "  cargo build            # Debug build"
              echo "  cargo build --release  # Release build"
              echo "  cargo run -- run --config <file>  # Create or attach to a session"
              echo
              echo "Test & lint:"
              echo "  cargo test             # Run all unit tests"
              echo "  cargo clippy           # Run linter"
              echo "  cargo fmt              # Format source code"
              echo
              echo "Nix:"
              echo "  nix build              # Build the package via Nix"
              echo "  nix run . -- run --config <file>  # Run via Nix (uses release build)"
              echo "  nix flake check        # Run every CI gate locally (build, test, clippy, fmt)"
              echo
            '';
          };

          # ── Checks (everything CI enforces, runnable via `nix flake check`) ───
          checks = {
            # Compile the crate and run `cargo test` in a pure sandbox.
            build = pkg;

            # Clippy with warnings promoted to errors.
            clippy = pkg.overrideAttrs (old: {
              pname = "nix-tmux-define-clippy";
              nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.clippy ];
              buildPhase = "cargo clippy --all-targets --all-features -- -D warnings";
              doCheck = false;
              installPhase = "touch $out";
            });

            # Rust formatting.
            rustfmt = srcCheck "rustfmt" [ pkgs.rustfmt ] ''
              rustfmt --check --edition 2021 $(find . -name '*.rs')
            '';

            # Nix formatting.
            nixfmt = srcCheck "nixfmt" [ pkgs.nixfmt ] ''
              nixfmt --check $(find . -name '*.nix')
            '';

            # GitHub Actions workflow linting (shellcheck powers embedded scripts).
            # Files are passed explicitly because the sandbox copy has no `.git`
            # for actionlint to auto-discover the workflows directory.
            actionlint = srcCheck "actionlint" [ pkgs.actionlint pkgs.shellcheck ] ''
              actionlint -color .github/workflows/*.yml
            '';

            # Every shipped example must parse and validate with the real binary.
            examples = srcCheck "examples" [ pkg ] ''
              for f in examples/*.json examples/*.toml examples/*.yaml; do
                echo "validating $f"
                nix-tmux-define validate --config "$f"
              done
            '';

            # Evaluate the exported Home Manager module with representative
            # valid and invalid session definitions.
            home-manager-module = homeManagerModuleCheck;
          };

          # ── Formatter (`nix fmt`) ─────────────────────────────────────────────
          formatter = pkgs.nixfmt;
        };

      # ── Home Manager Module ───────────────────────────────────────────────────
      flake.homeManagerModules = {
        nix-tmux-define = import ./module.nix { inherit self; };
        default = import ./module.nix { inherit self; };
      };
    };
}
