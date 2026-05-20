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
              nixd
            ];
            shellHook = ''
              cat <<'USAGE_EOF'

=== nix-tmux-define Development Shell ===

Build & run:
  cargo build            # Debug build
  cargo build --release  # Release build
  cargo run -- <file>    # Run with a JSON session file

Test & lint:
  cargo test             # Run all unit tests
  cargo clippy           # Run linter
  cargo fmt              # Format source code

Nix:
  nix build              # Build the package via Nix
  nix run -- <file>      # Run via Nix (uses release build)
  nix flake check        # Run checks in sandbox

USAGE_EOF
            '';
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
