use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "nix-tmux-define",
    about = "Declarative tmux session manager — generates a bash script from a JSON config",
    version
)]
struct Cli {
    /// Path to the JSON session config file
    #[arg(long, value_name = "PATH")]
    config: PathBuf,

    /// Print the generated script to stdout without executing
    #[arg(long)]
    print: bool,
}

// ─── Data Model ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Session {
    pub name: String,
    /// Default working directory for all panes; falls back to $HOME
    pub root: Option<String>,
    pub windows: Vec<Window>,
    /// Environment variables applied via `tmux set-environment`
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Command to run before creating the session (e.g. a build step)
    pub pre_hook: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Window {
    pub name: String,
    pub layout: LayoutNode,
    /// Per-window working directory; overrides session root
    pub root: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

/// Recursive layout tree: either a split node or a leaf pane.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LayoutNode {
    Pane {
        /// Shell command to send to this pane on startup
        command: Option<String>,
        /// If true, this pane will be focused after session creation
        #[serde(default)]
        focus: bool,
    },
    Split {
        direction: Direction,
        /// Fraction of space the *first* child receives (0.0 – 1.0)
        ratio: f64,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Horizontal,
    Vertical,
}

// ─── Script Compiler ─────────────────────────────────────────────────────────

pub struct Compiler {
    lines: Vec<String>,
    pane_counter: usize,
    /// Pane variable name that should receive focus
    focus_pane: Option<String>,
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            pane_counter: 0,
            focus_pane: None,
        }
    }

    fn emit(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    fn next_pane_var(&mut self) -> String {
        let v = format!("PANE_{}", self.pane_counter);
        self.pane_counter += 1;
        v
    }

    pub fn compile(&mut self, session: &Session) {
        let root = session.root.as_deref().unwrap_or("$HOME");

        self.emit("#!/usr/bin/env bash");
        self.emit("set -euo pipefail");
        self.emit("");
        self.emit(format!("SESSION={}", shell_quote(&session.name)));
        self.emit(format!("ROOT={}", shell_quote(root)));
        self.emit("");

        // Idempotency: attach if session already exists
        self.emit(r#"if tmux has-session -t "$SESSION" 2>/dev/null; then"#);
        self.emit(r#"  exec tmux attach-session -t "$SESSION""#);
        self.emit("fi");
        self.emit("");

        if let Some(hook) = &session.pre_hook {
            self.emit("# pre_hook");
            self.emit(hook.clone());
            self.emit("");
        }

        // Session-level environment variables
        for ev in &session.env {
            self.emit(format!(
                "export {}={}",
                ev.key,
                shell_quote(&ev.value)
            ));
        }
        if !session.env.is_empty() {
            self.emit("");
        }

        for (idx, window) in session.windows.iter().enumerate() {
            let win_root = window
                .root
                .as_deref()
                .or(session.root.as_deref())
                .unwrap_or("$HOME");
            self.compile_window(window, idx, win_root);
            self.emit("");
        }

        self.emit(r#"tmux select-window -t "$SESSION:0""#);
        self.emit(r#"exec tmux attach-session -t "$SESSION""#);
    }

    fn compile_window(&mut self, window: &Window, index: usize, root: &str) {
        self.emit(format!("# ── window {}: {} ──", index, window.name));
        self.focus_pane = None;

        let initial_pane = if index == 0 {
            let var = self.next_pane_var();
            self.emit(format!(
                "{}=$(tmux new-session -d -s \"$SESSION\" -c {} -n {} -P -F '#{{pane_id}}')",
                var,
                shell_quote(root),
                shell_quote(&window.name),
            ));
            var
        } else {
            let var = self.next_pane_var();
            self.emit(format!(
                "{}=$(tmux new-window -t \"$SESSION\" -c {} -n {} -P -F '#{{pane_id}}')",
                var,
                shell_quote(root),
                shell_quote(&window.name),
            ));
            var
        };

        self.visit_node(&window.layout, &initial_pane, root);

        if let Some(ref fp) = self.focus_pane.clone() {
            self.emit(format!("tmux select-pane -t \"${{{}}}\"", fp));
        }
    }

    fn visit_node(&mut self, node: &LayoutNode, current_pane: &str, root: &str) {
        match node {
            LayoutNode::Pane { command, focus } => {
                if let Some(cmd) = command {
                    self.emit(format!(
                        "tmux send-keys -t \"${{{}}}\" {} Enter",
                        current_pane,
                        shell_quote(cmd),
                    ));
                }
                if *focus {
                    self.focus_pane = Some(current_pane.to_string());
                }
            }

            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let new_pane = self.next_pane_var();
                let flag = match direction {
                    Direction::Horizontal => "-h",
                    Direction::Vertical => "-v",
                };
                // -l specifies the size of the *new* (second) pane
                let size_pct = ((1.0 - ratio).clamp(0.0, 1.0) * 100.0).round() as u32;
                self.emit(format!(
                    "{}=$(tmux split-window -t \"${{{}}}\" {} -l {}% -c {} -P -F '#{{pane_id}}')",
                    new_pane,
                    current_pane,
                    flag,
                    size_pct,
                    shell_quote(root),
                ));
                self.visit_node(first, current_pane, root);
                self.visit_node(second, &new_pane, root);
            }
        }
    }

    pub fn into_script(self) -> String {
        self.lines.join("\n")
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// POSIX single-quote escaping: wrap in `'...'`, escaping interior `'` as `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ─── Entry Point ─────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    let raw = std::fs::read_to_string(&cli.config)
        .with_context(|| format!("Failed to read config file: {}", cli.config.display()))?;

    let session: Session = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse JSON in: {}", cli.config.display()))?;

    let mut compiler = Compiler::new();
    compiler.compile(&session);
    let script = compiler.into_script();

    println!("{}", script);

    if !cli.print {
        // Execute the generated script in-process via bash
        let status = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .status()
            .context("Failed to launch bash")?;

        if !status.success() {
            anyhow::bail!("Session script exited with: {}", status);
        }
    }

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn compile(session: &Session) -> String {
        let mut c = Compiler::new();
        c.compile(session);
        c.into_script()
    }

    // ── Deserialization ───────────────────────────────────────────────────────

    #[test]
    fn parse_pane_node() {
        let v = json!({ "type": "pane", "command": "vim", "focus": true });
        let node: LayoutNode = serde_json::from_value(v).unwrap();
        assert_eq!(
            node,
            LayoutNode::Pane {
                command: Some("vim".into()),
                focus: true
            }
        );
    }

    #[test]
    fn parse_pane_node_defaults() {
        let v = json!({ "type": "pane" });
        let node: LayoutNode = serde_json::from_value(v).unwrap();
        assert_eq!(
            node,
            LayoutNode::Pane {
                command: None,
                focus: false
            }
        );
    }

    #[test]
    fn parse_split_node() {
        let v = json!({
            "type": "split",
            "direction": "horizontal",
            "ratio": 0.6,
            "first":  { "type": "pane", "command": "vim" },
            "second": { "type": "pane", "command": "bash" }
        });
        let node: LayoutNode = serde_json::from_value(v).unwrap();
        match node {
            LayoutNode::Split { direction, ratio, .. } => {
                assert_eq!(direction, Direction::Horizontal);
                assert!((ratio - 0.6).abs() < f64::EPSILON);
            }
            _ => panic!("expected Split"),
        }
    }

    #[test]
    fn parse_full_spec_example() {
        let raw = r#"{
          "name": "dev-session",
          "root": "/home/user/src/project",
          "windows": [{
            "name": "main",
            "layout": {
              "type": "split",
              "direction": "horizontal",
              "ratio": 0.6,
              "first": {
                "type": "pane",
                "command": "emacsclient -c -a '' .",
                "focus": true
              },
              "second": {
                "type": "split",
                "direction": "vertical",
                "ratio": 0.5,
                "first":  { "type": "pane", "command": "cargo watch -x check", "focus": false },
                "second": { "type": "pane", "command": "git status", "focus": false }
              }
            }
          }]
        }"#;
        let session: Session = serde_json::from_str(raw).unwrap();
        assert_eq!(session.name, "dev-session");
        assert_eq!(session.windows.len(), 1);
        assert_eq!(session.windows[0].name, "main");
    }

    // ── Script Generation ─────────────────────────────────────────────────────

    fn single_pane_session() -> Session {
        Session {
            name: "test".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "main".into(),
                root: None,
                layout: LayoutNode::Pane {
                    command: Some("bash".into()),
                    focus: true,
                },
            }],
            env: vec![],
            pre_hook: None,
        }
    }

    #[test]
    fn script_has_shebang() {
        let script = compile(&single_pane_session());
        assert!(script.starts_with("#!/usr/bin/env bash"));
    }

    #[test]
    fn script_has_session_guard() {
        let script = compile(&single_pane_session());
        assert!(script.contains("tmux has-session"));
        assert!(script.contains("attach-session"));
    }

    #[test]
    fn script_creates_session() {
        let script = compile(&single_pane_session());
        assert!(script.contains("tmux new-session"));
        assert!(script.contains("'test'"));
    }

    #[test]
    fn script_sends_command() {
        let script = compile(&single_pane_session());
        assert!(script.contains("tmux send-keys"));
        assert!(script.contains("'bash'"));
    }

    #[test]
    fn script_selects_focus_pane() {
        let script = compile(&single_pane_session());
        assert!(script.contains("tmux select-pane"));
    }

    #[test]
    fn script_horizontal_split_ratio() {
        let session = Session {
            name: "s".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                layout: LayoutNode::Split {
                    direction: Direction::Horizontal,
                    ratio: 0.6,
                    first: Box::new(LayoutNode::Pane { command: None, focus: false }),
                    second: Box::new(LayoutNode::Pane { command: None, focus: false }),
                },
            }],
            env: vec![],
            pre_hook: None,
        };
        let script = compile(&session);
        // second pane gets 40% (100 - 60)
        assert!(script.contains("-h"), "should use -h for horizontal");
        assert!(script.contains("40%"), "second pane should be 40%");
    }

    #[test]
    fn script_vertical_split_ratio() {
        let session = Session {
            name: "s".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                layout: LayoutNode::Split {
                    direction: Direction::Vertical,
                    ratio: 0.3,
                    first: Box::new(LayoutNode::Pane { command: None, focus: false }),
                    second: Box::new(LayoutNode::Pane { command: None, focus: false }),
                },
            }],
            env: vec![],
            pre_hook: None,
        };
        let script = compile(&session);
        assert!(script.contains("-v"), "should use -v for vertical");
        assert!(script.contains("70%"), "second pane should be 70%");
    }

    #[test]
    fn script_nested_split_three_panes() {
        let raw = r#"{
          "name": "dev",
          "root": "/project",
          "windows": [{
            "name": "main",
            "layout": {
              "type": "split",
              "direction": "horizontal",
              "ratio": 0.6,
              "first":  { "type": "pane", "command": "emacs", "focus": true },
              "second": {
                "type": "split",
                "direction": "vertical",
                "ratio": 0.5,
                "first":  { "type": "pane", "command": "cargo watch" },
                "second": { "type": "pane", "command": "git log" }
              }
            }
          }]
        }"#;
        let session: Session = serde_json::from_str(raw).unwrap();
        let script = compile(&session);

        // Should create 2 split-window calls (3 panes total)
        let split_count = script.matches("split-window").count();
        assert_eq!(split_count, 2, "3 panes need exactly 2 splits");

        // All three commands should appear
        assert!(script.contains("'emacs'"));
        assert!(script.contains("'cargo watch'"));
        assert!(script.contains("'git log'"));

        // Focus pane is emacs
        assert!(script.contains("select-pane -t \"${PANE_0}\""));
    }

    #[test]
    fn script_multiple_windows() {
        let session = Session {
            name: "multi".into(),
            root: Some("/tmp".into()),
            windows: vec![
                Window {
                    name: "code".into(),
                    root: None,
                    layout: LayoutNode::Pane { command: Some("vim".into()), focus: false },
                },
                Window {
                    name: "shell".into(),
                    root: None,
                    layout: LayoutNode::Pane { command: Some("bash".into()), focus: false },
                },
            ],
            env: vec![],
            pre_hook: None,
        };
        let script = compile(&session);
        assert!(script.contains("tmux new-session"), "first window uses new-session");
        assert!(script.contains("tmux new-window"), "second window uses new-window");
        assert!(script.contains("'code'"));
        assert!(script.contains("'shell'"));
    }

    #[test]
    fn script_env_vars() {
        let session = Session {
            name: "env-test".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                layout: LayoutNode::Pane { command: None, focus: false },
            }],
            env: vec![
                EnvVar { key: "EDITOR".into(), value: "nvim".into() },
                EnvVar { key: "PAGER".into(), value: "less".into() },
            ],
            pre_hook: None,
        };
        let script = compile(&session);
        assert!(script.contains("export EDITOR='nvim'"));
        assert!(script.contains("export PAGER='less'"));
    }

    #[test]
    fn script_pre_hook() {
        let session = Session {
            name: "hook-test".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                layout: LayoutNode::Pane { command: None, focus: false },
            }],
            env: vec![],
            pre_hook: Some("nix build".into()),
        };
        let script = compile(&session);
        assert!(script.contains("nix build"));
        // pre_hook should appear before new-session
        let hook_pos = script.find("nix build").unwrap();
        let session_pos = script.find("new-session").unwrap();
        assert!(hook_pos < session_pos, "pre_hook runs before session creation");
    }

    // ── shell_quote ──────────────────────────────────────────────────────────

    #[test]
    fn quote_simple_string() {
        assert_eq!(shell_quote("hello"), "'hello'");
    }

    #[test]
    fn quote_string_with_spaces() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }

    #[test]
    fn quote_string_with_single_quote() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn quote_emacs_command() {
        // emacsclient -c -a '' .   →  'emacsclient -c -a '\''\'' .'
        let q = shell_quote("emacsclient -c -a '' .");
        // Verify it round-trips through bash by checking structure
        assert!(q.starts_with('\''));
        assert!(q.contains("'\\''"));
    }

    // ── Integration: JSON file round-trip ────────────────────────────────────

    #[test]
    fn roundtrip_serialize_deserialize() {
        let original = Session {
            name: "rt".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w1".into(),
                root: None,
                layout: LayoutNode::Split {
                    direction: Direction::Vertical,
                    ratio: 0.4,
                    first: Box::new(LayoutNode::Pane {
                        command: Some("top".into()),
                        focus: true,
                    }),
                    second: Box::new(LayoutNode::Pane {
                        command: Some("htop".into()),
                        focus: false,
                    }),
                },
            }],
            env: vec![EnvVar { key: "FOO".into(), value: "bar".into() }],
            pre_hook: None,
        };
        let json = serde_json::to_string_pretty(&original).unwrap();
        let parsed: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(original, parsed);
    }
}
