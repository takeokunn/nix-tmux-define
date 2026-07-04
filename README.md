# nix-tmux-define

[![CI](https://github.com/takeokunn/nix-tmux-define/actions/workflows/ci.yml/badge.svg)](https://github.com/takeokunn/nix-tmux-define/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

**Declarative, type-safe tmux session manager powered by Nix + Rust.**

Define your tmux workspace once in Nix (or JSON) and reproduce it instantly — no Python runtime, no mutable state, no drift.

---

## Why nix-tmux-define?

| | tmuxp / tmuxinator | Pure Nix | **nix-tmux-define** |
|---|---|---|---|
| Runtime dependency | Python / Ruby | Nix evaluator | **Rust binary only** |
| Config language | YAML / Ruby | Nix attrsets | **Nix → JSON → Rust** |
| Recursive layouts | Limited | Complex to compute | **Tree → DFS → bash** |
| Home Manager integration | Manual | N/A | **Native HM module** |
| Reproducibility | ✗ | ✓ | **✓** |

### Architecture

```mermaid
flowchart TD
    subgraph NixLayer["Nix / Home Manager layer"]
        HMConfig["home.nix\nsessions.*.configPath"]
        RuntimeConfig["runtime config\n$HOME/.config/nix-tmux-define/*.json"]
        ModuleNix["module.nix\nlauncher + activation validation"]
        ShellScript["tmux-session-&lt;name&gt;\n(shell script in PATH)"]

        HMConfig -->|"absolute path only"| ModuleNix
        ModuleNix -->|"pkgs.writeShellScriptBin"| ShellScript
    end

    subgraph RustCLI["Rust CLI  (nix-tmux-define)"]
        Format["format.rs\nload_session()\nJSON / TOML / YAML"]
        Model["model.rs\nSession → Window → LayoutNode"]

        subgraph PrintPath["print path"]
            Compiler["compiler.rs\nCompiler\nDFS → bash text"]
            BashScript["#!/usr/bin/env bash\nsplit-window …\nsend-keys …"]
        end

        subgraph RunPath["run / reload path"]
            Executor["executor.rs\nExecutor"]
            Backend["backend.rs\nTmuxBackend trait"]
            RealTmux["RealTmux\n(production)"]
            Recording["RecordingBackend\n(tests)"]
        end

        Format --> Model
        Model --> Compiler
        Compiler --> BashScript
        Model --> Executor
        Executor --> Backend
        Backend --> RealTmux
        Backend -.->|"test double"| Recording
    end

    ShellScript -->|"CLI run/reload/print/validate\n--config PATH"| Format
    RuntimeConfig -->|"loaded at runtime"| Format
    BashScript -->|"exec bash"| Tmux[("tmux server")]
    RealTmux -->|"tmux new-session\nsplit-window\nsend-keys"| Tmux
```

The Rust CLI has **two execution paths**:

- **`print` path** — `Compiler` performs a two-phase depth-first traversal of the `LayoutNode` tree, emitting all `split-window` calls first (structure phase), then all `send-keys` / `select-pane` calls (command phase). This is for dry-runs and explicit script export.
- **`run` / `reload` path** — `Executor` drives tmux directly via the `TmuxBackend` trait, bypassing the bash script entirely. Home Manager launchers use this path by default. `RecordingBackend` implements the same trait for unit and integration tests without spawning a real tmux.

This guarantees every pane is ready before any command is dispatched.

---

## Quick Start

### Try without installing

```bash
nix run github:takeokunn/nix-tmux-define -- print --config ./session.json
```

Ready-to-run configs live in [`examples/`](examples/) — one per supported format
(JSON, TOML, YAML).

### Add to your flake

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url        = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager.url   = "github:nix-community/home-manager";
    nix-tmux-define.url = "github:takeokunn/nix-tmux-define";
  };

  outputs = { home-manager, nix-tmux-define, ... }: {
    homeConfigurations."you@host" = home-manager.lib.homeManagerConfiguration {
      modules = [
        nix-tmux-define.homeManagerModules.default
        ./home.nix
      ];
    };
  };
}
```

### Configure in `home.nix`

```nix
programs.nix-tmux-define = {
  enable = true;

  sessions.dev = {
    configPath = "${config.home.homeDirectory}/.config/nix-tmux-define/dev.json";
    commandName = "tmux-session-dev";
  };
};
```

Keep the session body in a runtime config file, not in the Nix store:

```jsonc
// ~/.config/nix-tmux-define/dev.json
{
  "name": "dev",
  "root": "~/src/myproject",
  "env": [{ "key": "EDITOR", "value": "nvim" }],
  "windows": [
    {
      "name": "main",
      "layout": {
        "type": "split",
        "direction": "horizontal",
        "ratio": 0.6,
        "first": { "type": "pane", "command": "nvim .", "focus": true },
        "second": {
          "type": "split",
          "direction": "vertical",
          "ratio": 0.5,
          "first": { "type": "pane", "command": "cargo watch -x check" },
          "second": { "type": "pane", "command": "git status", "title": "git" }
        }
      }
    },
    {
      "name": "logs",
      "layout": { "type": "pane", "command": "journalctl -f" }
    }
  ]
}
```

After `home-manager switch`, a `tmux-session-dev` command appears in your PATH:

```bash
tmux-session-dev            # create session (or reattach if it exists)
tmux-session-dev --reload   # replace only the dev session
tmux-session-dev -r         # shorthand for --reload
tmux-session-dev --print    # print the generated bash script
tmux-session-dev --validate # validate the referenced config
```

### Dynamically generating sessions

Because `sessions` is a plain Nix `attrsOf`, you can generate entries with `map` and `lib.listToAttrs`:

```nix
let
  profiles = [
    { name = "api";      file = "api.json"; }
    { name = "frontend"; file = "frontend.json"; }
  ];
in {
  programs.nix-tmux-define.sessions =
    lib.listToAttrs (map (p: lib.nameValuePair p.name {
      configPath = "${config.home.homeDirectory}/.config/nix-tmux-define/${p.file}";
      commandName = "tmux-${p.name}";
    }) profiles);
}
```

---

## JSON Config Reference

The Rust CLI accepts JSON, TOML, and YAML files with the same session model.
The Home Manager module only points at these files; it does not serialize session
contents into `/nix/store`.

### Session

```jsonc
{
  "name":     "dev-session",          // tmux session name (required)
  "root":     "/home/user/src/proj",  // default working dir (optional)
  "env":      [{ "key": "K", "value": "V" }],  // session-level exports
  "pre_hook": "nix build",            // runs before new-session
  "options":  { "status": "off" },    // tmux set-option key/value pairs
  "vars":     { "proj": "/home/user/src" },     // template variables
  "windows":  [ /* Window[] */ ]
}
```

### Window

```jsonc
{
  "name":          "main",
  "root":          "/override",   // overrides session root for this window
  "env":           [],            // window-scoped exports
  "options":       { "synchronize-panes": "on" },  // tmux set-window-option
  "select_layout": "tiled",       // apply a tmux layout preset (optional)
  "layout":        /* LayoutNode */
}
```

### LayoutNode — Pane (leaf)

```jsonc
{
  "type":     "pane",
  "command":  "nvim .",     // sent via send-keys (optional)
  "focus":    true,         // move focus here after setup (default: false)
  "title":    "editor",     // select-pane -T (optional)
  "wait_for": {             // block until pattern appears in pane output
    "pattern": "ready",
    "timeout": 30           // seconds (default: 30)
  }
}
```

### LayoutNode — Split (branch)

```jsonc
{
  "type":      "split",
  "direction": "horizontal",  // "horizontal" | "vertical"
  "ratio":     0.6,           // first child gets 60%, second 40% (strictly 0–1)
  "first":     { /* LayoutNode */ },
  "second":    { /* LayoutNode */ }
}
```

> **`direction` semantics**
> - `horizontal` → side-by-side panes (`split-window -h`)
> - `vertical`   → top/bottom panes (`split-window -v`)

### Template variables

Use `{{key}}` placeholders in `command` and `root` values.
Built-in variables are always available:

| Placeholder | Expands to |
|---|---|
| `{{cwd}}` | `$PWD` at session-start time |
| `{{date}}` | `$(date +%Y-%m-%d)` |
| `{{git_branch}}` | `$(git rev-parse --abbrev-ref HEAD)` |

User-defined variables go in the `vars` map at session level.

### Full example

```json
{
  "name": "dev-session",
  "root": "/home/user/src/project",
  "env":  [{ "key": "EDITOR", "value": "nvim" }],
  "windows": [
    {
      "name": "main",
      "layout": {
        "type": "split", "direction": "horizontal", "ratio": 0.6,
        "first":  { "type": "pane", "command": "nvim .", "focus": true, "title": "editor" },
        "second": {
          "type": "split", "direction": "vertical", "ratio": 0.5,
          "first":  { "type": "pane", "command": "cargo watch -x check" },
          "second": { "type": "pane", "command": "git log --oneline" }
        }
      }
    },
    {
      "name": "shell",
      "layout": { "type": "pane" }
    }
  ]
}
```

---

## CLI Reference

```
nix-tmux-define <COMMAND>

Commands:
  run          Start a tmux session from a config file
  print        Print the generated bash script to stdout (dry-run)
  reload       Atomically replace a named session from a config file
  validate     Parse a config and report errors
  list         List sessions from config files without probing tmux by default
  schema       Print the JSON Schema for the session config format
  completions  Emit shell completion scripts

Options:
  -h, --help     Print help
  -V, --version  Print version
```

### `run`

```
nix-tmux-define run --config <PATH>

Options:
  --config <PATH>   Path to the session config (JSON, TOML, or YAML)
  --no-attach       Build the session but do not attach or switch to it
```

Attaching is **terminal-aware**: inside tmux it switches the current client, with
a terminal on stdin it attaches, and with no terminal (a systemd oneshot, a CI
step, any non-interactive caller) it builds the session detached and exits `0`
instead of failing with `open terminal failed: not a terminal`. Pass
`--no-attach` to stay detached even from a terminal — handy for preseeding
sessions in the background.

### `reload`

```
nix-tmux-define reload --config <PATH>

Options:
  --config <PATH>   Path to the session config (JSON, TOML, or YAML)
  --no-attach       Reload the session but do not attach or switch to it
```

Builds a replacement session, swaps it into the configured session name, then
removes the old session. If replacement creation fails, the existing session is
left in place. Attaching follows the same terminal-aware rules as `run`.

### `list`

```
nix-tmux-define list [--config <PATH>]... [--config-dir <DIR>] [--running-status]
```

By default, `list` only reads configuration files. Add `--running-status` to
explicitly probe `tmux list-sessions` and append `[running]` to matching
sessions.

Directory scanning is strict where you ask for it and lenient where it is
implicit:

- With no `--config`/`--config-dir`, `list` scans the **current directory** as a
  best-effort convenience. Config-extension files that are not session configs
  (`Cargo.toml`, `package.json`, `tsconfig.json`, …) are skipped with a
  `warning:` on stderr instead of aborting the command.
- With an explicit `--config-dir <DIR>`, every `.json`/`.toml`/`.yaml`/`.yml`
  file must parse and validate as a session; a malformed one is a hard error, so
  problems in a directory you curated are surfaced rather than hidden.

### Examples

```bash
# Dry-run — inspect the generated script
nix-tmux-define print --config session.json

# Create session (or reattach if already running)
nix-tmux-define run --config session.json

# Replace only this session
nix-tmux-define reload --config session.json

# Validate config without touching tmux
nix-tmux-define validate --config session.json

# List sessions in current directory
nix-tmux-define list

# Install fish completions
nix-tmux-define completions fish > ~/.config/fish/completions/nix-tmux-define.fish
```

### Idempotency & nested tmux

The generated script is always idempotent:

```bash
if tmux has-session -t "$SESSION" 2>/dev/null; then
  if [ -n "${TMUX:-}" ]; then
    exec tmux switch-client -t "$SESSION"   # already inside tmux
  elif [ -t 0 ]; then
    exec tmux attach-session -t "$SESSION"  # fresh terminal
  else
    echo "session '$SESSION' is ready (not attaching; no terminal)." >&2
    exit 0                                  # headless: systemd, CI
  fi
fi
```

Running `tmux-session-dev` twice never creates a duplicate; it just reattaches —
or, with no terminal, leaves the session ready and exits cleanly.

---

## Development

```bash
# Enter the dev shell (provides cargo, rustc, rust-analyzer, tmux)
nix develop

# Run tests
cargo test

# Run clippy
cargo clippy

# Build the release binary via Nix
nix build

# Check all outputs (runs cargo test in a pure sandbox)
nix flake check
```

### Project layout

```
nix-tmux-define/
├── src/
│   ├── lib.rs        # public API surface
│   ├── main.rs       # CLI entry point (clap subcommands)
│   ├── model.rs      # Session / Window / LayoutNode types + serde
│   ├── compiler.rs   # Compiler: Session → bash script (print path)
│   ├── executor.rs   # Executor: Session → live tmux calls (run path)
│   ├── backend.rs    # TmuxBackend trait + RealTmux + RecordingBackend
│   └── format.rs     # load_session(): JSON / TOML / YAML deserialization
├── flake.nix         # packages, apps, devShell, checks, homeManagerModules
├── module.nix        # Home Manager option definitions
└── Cargo.toml
```

---

## Contributing

Bug reports and pull requests are welcome! See **[CONTRIBUTING.md](CONTRIBUTING.md)**
for the full workflow and **[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)** for community
expectations. Security issues should follow **[SECURITY.md](SECURITY.md)**.

The short version:

1. Fork the repository and `nix develop` to enter the dev shell.
2. Make changes and add tests (cover the compiler **and** executor paths).
3. Run `nix flake check` — it must be green. This single command runs the same
   gates as CI: build, tests, clippy (`-D warnings`), rustfmt, nixfmt, actionlint,
   and example validation.
4. Open a PR and update `CHANGELOG.md` under "Unreleased".

---

## License

[MIT](LICENSE) © takeokunn
