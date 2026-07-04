# Examples

Ready-to-run session configs, one per supported format. Every file here is
validated in CI (`nix flake check`), so they always parse.

| File | Format | Highlights |
|---|---|---|
| [`dev.json`](dev.json) | JSON | Nested splits, per-pane commands, focus, titles, session env |
| [`simple.toml`](simple.toml) | TOML | Minimal single-window session |
| [`monitoring.yaml`](monitoring.yaml) | YAML | Template vars, `pre_hook`, options, `select_layout`, `wait_for` |

## Try them

```bash
# Inspect the generated bash script (no tmux needed)
nix-tmux-define print --config examples/dev.json

# Validate without touching tmux
nix-tmux-define validate --config examples/monitoring.yaml

# Actually start the session
nix-tmux-define run --config examples/simple.toml
```

When trying these for real, adjust `root` (e.g. `~/src/project`) to a directory
that exists on your machine.
