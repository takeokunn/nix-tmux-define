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
        let
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
        in
        {
          # ── Packages ────────────────────────────────────────────────────────
          packages.default = pkg;
          packages.nix-tmux-define = pkg;

          # ── App (nix run) ────────────────────────────────────────────────────
          apps.default = {
            type = "app";
            program = "${pkg}/bin/nix-tmux-define";
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
            ];
          };

          # ── Checks ────────────────────────────────────────────────────────────
          checks.tests = pkgs.rustPlatform.buildRustPackage {
            pname = "nix-tmux-define-tests";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            doCheck = true;
            installPhase = "mkdir -p $out";
          };

          # ── Formatter ─────────────────────────────────────────────────────────
          formatter = pkgs.nixfmt;
        };

      # ── Home Manager Module ───────────────────────────────────────────────────
      flake.homeManagerModules = {
        nix-tmux-define = import ./module.nix { inherit self; };
        default = import ./module.nix { inherit self; };
      };
    };
}
