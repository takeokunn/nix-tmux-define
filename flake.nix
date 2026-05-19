{
  description = "nix-tmux-define — declarative tmux session manager (Nix + Rust hybrid)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    { self, nixpkgs, flake-parts, ... }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      perSystem =
        { pkgs, system, ... }:
        {
          # ── Package ──────────────────────────────────────────────────────────
          packages.default = pkgs.rustPlatform.buildRustPackage {
            pname = "nix-tmux-define";
            version = "0.1.0";
            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            meta = {
              description = "Declarative tmux session manager driven by JSON / Home Manager";
              homepage = "https://github.com/takeokunn/nix-tmux-define";
              license = pkgs.lib.licenses.mit;
              mainProgram = "nix-tmux-define";
            };
          };

          packages.nix-tmux-define = self.packages.${system}.default;

          # ── Dev Shell ─────────────────────────────────────────────────────────
          devShells.default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rust-analyzer
              clippy
              rustfmt
              tmux
            ];
          };

          # ── Checks ────────────────────────────────────────────────────────────
          checks.tests = pkgs.rustPlatform.buildRustPackage {
            pname = "nix-tmux-define-tests";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            # Run `cargo test` during the check build
            buildPhase = "cargo test --release 2>&1";
            installPhase = "mkdir -p $out";
          };

          # ── Formatter ─────────────────────────────────────────────────────────
          formatter = pkgs.nixfmt-rfc-style;
        };

      # ── Home Manager Module (system-independent) ──────────────────────────
      flake.homeManagerModules = {
        nix-tmux-define = import ./module.nix { inherit self; };
        default = import ./module.nix { inherit self; };
      };
    };
}
