# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `examples/` directory with ready-to-run JSON, TOML, and YAML session configs.
- Community health files: `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`,
  issue templates, and a pull-request template.
- Crate-level documentation and a `CHANGELOG.md`.
- Convenience constructors `LayoutNode::pane()` / `command()` / `split()` and a
  `Default` impl.
- Boundary validation: split `ratio` must be finite and strictly within
  `(0.0, 1.0)`, and a session must define at least one window — both rejected at
  parse time across JSON/TOML/YAML.
- Tiered test layout: `tests/unit/` (proptest property tests), `tests/integration/`
  (the real tmux backend, isolated via `TMUX_TMPDIR`), and `tests/e2e/` (the
  compiled binary). The `RealTmux` backend is now covered end-to-end.

### Changed
- CI now runs entirely through Nix: `nix flake check` enforces build, tests,
  clippy (`-D warnings`), rustfmt, nixfmt, actionlint, and example validation as
  flake checks, so local and CI results cannot diverge.
- Dependabot keeps Cargo dependencies and GitHub Actions up to date.
- Bumped `actions/checkout` to v7 and `cachix/cachix-action` to v17 in CI and
  release workflows.
- Release builds now enable LTO, a single codegen unit, and debuginfo
  stripping (`[profile.release]` in `Cargo.toml`) for smaller, faster binaries.
- **The compiler and executor now share one typed `LayoutPlan`.** The two paths
  previously walked the layout tree with their own near-identical recursive
  functions — the structural root cause of the fixed divergence bugs. Both now
  build and render a single, backend-agnostic plan (splits + per-leaf config with
  a whole `Option<WaitFor>` instead of loose "has_wait_for/pattern/timeout"
  fields), so structural disagreement between `print` and `run` is no longer
  representable. New property tests assert both paths render the plan identically
  across hundreds of random sessions.

### Fixed
- **`list` no longer aborts on unrelated config files in the current directory.**
  The implicit current-directory scan (running `list` with no `--config` /
  `--config-dir`) previously failed outright when the directory held any
  `.json`/`.toml`/`.yaml`/`.yml` file that was not a session config — e.g.
  `Cargo.toml` or `package.json`, which are present in almost every project. Such
  files are now skipped with a `warning:` on stderr. An explicitly requested
  `--config-dir` stays strict and still errors on a malformed config.
- **The `run` / `reload` path now applies `env` and per-window `env`.** Previously
  only the `print` (compiler) path exported them, so starting a session via the
  executor — including the Home Manager `--reload` wrapper — silently dropped all
  configured environment variables.
- **The generated script no longer aborts when a window sets `options` or
  `select_layout`.** The compiler had embedded literal quotes in the tmux target
  (`"$SESSION:'name'"`), which tmux rejected as a nonexistent window, killing the
  `set -euo pipefail` script before it attached.
- Resolved a `clippy::items_after_test_module` lint in `src/main.rs` and applied
  `cargo fmt` across the tree.

## [0.1.0]

### Added
- Initial release: declarative tmux session manager driven by JSON / TOML / YAML
  or a Home Manager module.
- Two execution paths: `print` (compile to a bash script) and `run` / `reload`
  (drive tmux directly via the `TmuxBackend` trait).
- Recursive split layouts, per-window/session env vars and options, `pre_hook`,
  template variables (`{{cwd}}`, `{{date}}`, `{{git_branch}}`, and user-defined),
  `wait_for`, `select_layout`, and shell completions.
- Home Manager module generating `tmux-session-<name>` commands.

[Unreleased]: https://github.com/takeokunn/nix-tmux-define/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/takeokunn/nix-tmux-define/releases/tag/v0.1.0
