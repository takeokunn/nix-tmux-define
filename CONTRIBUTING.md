# Contributing to nix-tmux-define

Thanks for your interest in contributing! This project is a small, focused
Rust + Nix tool, and contributions of all kinds — bug reports, documentation,
features, and reviews — are welcome.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By
participating, you agree to uphold it.

## Getting started

The entire toolchain is provided through Nix, so you do not need a system-wide
Rust installation.

```bash
# Enter the dev shell (cargo, rustc, clippy, rustfmt, tmux, nixfmt, actionlint)
nix develop
```

If you use [direnv](https://direnv.net/), `direnv allow` will load the dev
shell automatically via the bundled `.envrc`.

## Development workflow

```bash
cargo build            # debug build
cargo run -- print --config examples/dev.json   # try it out
cargo test             # run the unit tests
cargo clippy           # lint
cargo fmt              # format
```

### Architecture in 30 seconds

| File | Responsibility |
|---|---|
| `src/model.rs` | `Session` / `Window` / `LayoutNode` types, serde, validators |
| `src/format.rs` | `load_session()` — JSON / TOML / YAML deserialization |
| `src/compiler.rs` | `Compiler` — turns a `Session` into a bash script (`print` path) |
| `src/executor.rs` | `Executor` — drives tmux directly via a backend (`run` path) |
| `src/backend.rs` | `TmuxBackend` trait + `RealTmux` + `RecordingBackend` (tests) |
| `src/main.rs` | CLI entry point (clap subcommands) |

The compiler and executor share the same two-phase layout traversal: build the
full pane structure first, then send commands. When you add a layout feature,
keep both paths in sync and add tests on each side (the `RecordingBackend`
makes executor tests fast and tmux-free).

## Before you open a pull request

Run the same gate CI runs — a single command checks build, tests, clippy,
rustfmt, nixfmt, and actionlint:

```bash
nix flake check
```

Everything must be green. In particular:

- `cargo fmt` leaves no diff (`rustfmt` check).
- `cargo clippy -- -D warnings` is clean (no warnings).
- New behavior is covered by tests.
- `.nix` files are formatted with `nixfmt` (`nix fmt`).

## Pull request guidelines

1. Fork the repository and create a topic branch.
2. Keep changes focused; one logical change per PR.
3. Write a clear description of **what** changed and **why**.
4. Reference any related issue (e.g. `Closes #12`).
5. Update `README.md` and `CHANGELOG.md` when behavior or the public interface
   changes.

## Commit messages

Follow the existing convention in `git log`, which loosely tracks
[Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add reload rollback
fix: suppress tmux stderr in session_running
docs: expand JSON config reference
security: harden input validation
```

## Reporting bugs and requesting features

Use the GitHub issue templates. For security issues, **do not** open a public
issue — see [SECURITY.md](SECURITY.md).
