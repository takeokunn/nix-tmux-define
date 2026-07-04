# Security Policy

## Supported versions

This project is pre-1.0. Security fixes are applied to the latest release and
the `main` branch.

| Version | Supported |
|---|---|
| `main` / latest | ✅ |
| older tags | ❌ |

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, use GitHub's private vulnerability reporting:

1. Go to the repository's **Security** tab.
2. Click **Report a vulnerability**.
3. Provide a description, reproduction steps, and the impact you observed.

You can expect an initial acknowledgement within a few days. Once the issue is
confirmed and fixed, we will coordinate disclosure and credit you (unless you
prefer to remain anonymous).

## Scope and threat model

`nix-tmux-define` reads a config file and generates a bash script (or drives
tmux directly) under the privileges of the invoking user. The most relevant
classes of issue are therefore:

- **Shell injection** via session/window names, commands, env values, titles,
  or template variables that are not correctly quoted before reaching bash or
  tmux.
- **Resource exhaustion** from pathological inputs (e.g. extremely deep layout
  trees — bounded by `MAX_LAYOUT_DEPTH`).

The tool does not run with elevated privileges and does not parse untrusted
network input. Config files are assumed to be authored by the user running the
tool; nonetheless, robust quoting is a hard requirement and regressions are
treated as security bugs.
