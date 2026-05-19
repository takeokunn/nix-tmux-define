use serde::{Deserialize, Serialize};

// ─── Model ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Session {
    pub name: String,
    /// Default working directory for all panes; falls back to `$HOME`
    pub root: Option<String>,
    pub windows: Vec<Window>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Shell command executed before the session is created (e.g. `nix build`)
    pub pre_hook: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Window {
    pub name: String,
    pub layout: LayoutNode,
    /// Working directory for this window; overrides the session root when set
    pub root: Option<String>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

/// A node in the recursive pane-layout tree.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LayoutNode {
    Pane {
        /// Command sent to this pane on startup via `send-keys`
        #[serde(default)]
        command: Option<String>,
        /// Moves focus to this pane after the session is fully built
        #[serde(default)]
        focus: bool,
        /// Sets the pane title via `select-pane -T`
        #[serde(default)]
        title: Option<String>,
    },
    Split {
        direction: Direction,
        /// Fraction of space [0.0, 1.0] allocated to the *first* child
        ratio: f64,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Horizontal,
    Vertical,
}

// ─── Shell Quoting ────────────────────────────────────────────────────────────

/// POSIX single-quote escape: wraps `s` in `'…'`, turning interior `'` into `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ─── Compiler ─────────────────────────────────────────────────────────────────

/// Metadata collected for each leaf pane during the structure-building phase.
struct PaneRecord {
    var: String,
    command: Option<String>,
    title: Option<String>,
    focus: bool,
}

/// Compiles a [`Session`] into a self-contained bash script.
///
/// Compilation is two-phased per window:
/// 1. Emit all `split-window` calls to build the pane tree.
/// 2. Emit `send-keys` / `select-pane` for each leaf pane.
///
/// This ensures every pane exists before any command is sent to it.
#[derive(Default)]
pub struct Compiler {
    lines: Vec<String>,
    pane_counter: usize,
}

impl Compiler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compile(&mut self, session: &Session) {
        self.emit_preamble(session);
        for (idx, window) in session.windows.iter().enumerate() {
            self.compile_window(window, idx, session);
        }
        self.emit("tmux select-window -t \"$SESSION:0\"");
        self.emit_attach_or_switch();
    }

    pub fn into_script(self) -> String {
        self.lines.join("\n")
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn emit(&mut self, s: impl Into<String>) {
        self.lines.push(s.into());
    }

    fn alloc_pane(&mut self) -> String {
        let name = format!("PANE_{}", self.pane_counter);
        self.pane_counter += 1;
        name
    }

    fn emit_preamble(&mut self, session: &Session) {
        self.emit("#!/usr/bin/env bash");
        self.emit("set -euo pipefail");
        self.emit("");
        self.emit(format!("SESSION={}", shell_quote(&session.name)));
        self.emit("");

        // Idempotency: reuse the existing session instead of failing
        self.emit("if tmux has-session -t \"$SESSION\" 2>/dev/null; then");
        self.emit("  if [ -n \"${TMUX:-}\" ]; then");
        self.emit("    exec tmux switch-client -t \"$SESSION\"");
        self.emit("  else");
        self.emit("    exec tmux attach-session -t \"$SESSION\"");
        self.emit("  fi");
        self.emit("fi");
        self.emit("");

        if let Some(hook) = &session.pre_hook {
            self.emit("# pre_hook");
            self.emit(hook.clone());
            self.emit("");
        }

        for ev in &session.env {
            self.emit(format!("export {}={}", ev.key, shell_quote(&ev.value)));
        }
        if !session.env.is_empty() {
            self.emit("");
        }
    }

    /// Emits the final attach/switch block, handling nested-tmux correctly.
    fn emit_attach_or_switch(&mut self) {
        self.emit("if [ -n \"${TMUX:-}\" ]; then");
        self.emit("  exec tmux switch-client -t \"$SESSION\"");
        self.emit("else");
        self.emit("  exec tmux attach-session -t \"$SESSION\"");
        self.emit("fi");
    }

    fn compile_window(&mut self, window: &Window, index: usize, session: &Session) {
        let root = window
            .root
            .as_deref()
            .or(session.root.as_deref())
            .unwrap_or("$HOME");

        self.emit(format!("# ── window {}: {} ──", index, window.name));

        for ev in &window.env {
            self.emit(format!("export {}={}", ev.key, shell_quote(&ev.value)));
        }

        // ── Phase 1: build pane structure ────────────────────────────────────
        let initial = if index == 0 {
            let var = self.alloc_pane();
            self.emit(format!(
                "{}=$(tmux new-session -d -s \"$SESSION\" -c {} -n {} -P -F '#{{pane_id}}')",
                var,
                shell_quote(root),
                shell_quote(&window.name),
            ));
            var
        } else {
            let var = self.alloc_pane();
            self.emit(format!(
                "{}=$(tmux new-window -t \"$SESSION\" -c {} -n {} -P -F '#{{pane_id}}')",
                var,
                shell_quote(root),
                shell_quote(&window.name),
            ));
            var
        };

        let mut records: Vec<PaneRecord> = Vec::new();
        self.collect_structure(&window.layout, &initial, root, &mut records);

        // ── Phase 2: send commands and configure panes ───────────────────────
        let mut focus_var: Option<String> = None;
        for rec in &records {
            if let Some(cmd) = &rec.command {
                self.emit(format!(
                    "tmux send-keys -t \"${{{}}}\" {} Enter",
                    rec.var,
                    shell_quote(cmd),
                ));
            }
            if let Some(title) = &rec.title {
                self.emit(format!(
                    "tmux select-pane -t \"${{{}}}\" -T {}",
                    rec.var,
                    shell_quote(title),
                ));
            }
            if rec.focus {
                focus_var = Some(rec.var.clone());
            }
        }
        if let Some(fp) = focus_var {
            self.emit(format!("tmux select-pane -t \"${{{}}}\"", fp));
        }

        self.emit("");
    }

    /// Recursively emits `split-window` commands (phase 1) and appends a
    /// [`PaneRecord`] for every leaf pane encountered.
    fn collect_structure(
        &mut self,
        node: &LayoutNode,
        current: &str,
        root: &str,
        records: &mut Vec<PaneRecord>,
    ) {
        match node {
            LayoutNode::Pane { command, focus, title } => {
                records.push(PaneRecord {
                    var: current.to_string(),
                    command: command.clone(),
                    title: title.clone(),
                    focus: *focus,
                });
            }
            LayoutNode::Split { direction, ratio, first, second } => {
                let new_pane = self.alloc_pane();
                let flag = match direction {
                    Direction::Horizontal => "-h",
                    Direction::Vertical => "-v",
                };
                // -l specifies the size of the *new* (second) pane
                let pct = ((1.0 - ratio).clamp(0.0, 1.0) * 100.0).round() as u32;
                self.emit(format!(
                    "{}=$(tmux split-window -t \"${{{}}}\" {} -l {}% -c {} -P -F '#{{pane_id}}')",
                    new_pane,
                    current,
                    flag,
                    pct,
                    shell_quote(root),
                ));
                self.collect_structure(first, current, root, records);
                self.collect_structure(second, &new_pane, root, records);
            }
        }
    }
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

    fn single_pane(name: &str, cmd: &str) -> Session {
        Session {
            name: name.into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "main".into(),
                root: None,
                env: vec![],
                layout: LayoutNode::Pane {
                    command: Some(cmd.into()),
                    focus: true,
                    title: None,
                },
            }],
            env: vec![],
            pre_hook: None,
        }
    }

    // ── shell_quote ───────────────────────────────────────────────────────────

    #[test]
    fn quote_plain() {
        assert_eq!(shell_quote("hello"), "'hello'");
    }

    #[test]
    fn quote_spaces() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }

    #[test]
    fn quote_single_quote() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn quote_emacs_command() {
        let q = shell_quote("emacsclient -c -a '' .");
        assert!(q.starts_with('\''));
        assert!(q.ends_with('\'') || q.ends_with("''"));
        // Round-trip sanity: contains the escaped single quotes
        assert!(q.contains("'\\''"));
    }

    // ── JSON deserialization ──────────────────────────────────────────────────

    #[test]
    fn parse_pane_defaults() {
        let node: LayoutNode = serde_json::from_value(json!({"type": "pane"})).unwrap();
        assert_eq!(node, LayoutNode::Pane { command: None, focus: false, title: None });
    }

    #[test]
    fn parse_pane_full() {
        let node: LayoutNode = serde_json::from_value(json!({
            "type": "pane",
            "command": "vim",
            "focus": true,
            "title": "editor"
        }))
        .unwrap();
        assert_eq!(
            node,
            LayoutNode::Pane { command: Some("vim".into()), focus: true, title: Some("editor".into()) }
        );
    }

    #[test]
    fn parse_split() {
        let node: LayoutNode = serde_json::from_value(json!({
            "type": "split",
            "direction": "horizontal",
            "ratio": 0.6,
            "first":  {"type": "pane"},
            "second": {"type": "pane"}
        }))
        .unwrap();
        match node {
            LayoutNode::Split { direction, ratio, .. } => {
                assert_eq!(direction, Direction::Horizontal);
                assert!((ratio - 0.6).abs() < f64::EPSILON);
            }
            _ => panic!("expected Split"),
        }
    }

    #[test]
    fn parse_spec_example() {
        let raw = r#"{
          "name": "dev-session",
          "root": "/home/user/src/project",
          "windows": [{
            "name": "main",
            "layout": {
              "type": "split", "direction": "horizontal", "ratio": 0.6,
              "first":  {"type": "pane", "command": "emacsclient -c -a '' .", "focus": true},
              "second": {
                "type": "split", "direction": "vertical", "ratio": 0.5,
                "first":  {"type": "pane", "command": "cargo watch -x check"},
                "second": {"type": "pane", "command": "git status"}
              }
            }
          }]
        }"#;
        let s: Session = serde_json::from_str(raw).unwrap();
        assert_eq!(s.name, "dev-session");
        assert_eq!(s.windows.len(), 1);
    }

    #[test]
    fn roundtrip_serde() {
        let original = Session {
            name: "rt".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![EnvVar { key: "FOO".into(), value: "bar".into() }],
                layout: LayoutNode::Split {
                    direction: Direction::Vertical,
                    ratio: 0.4,
                    first: Box::new(LayoutNode::Pane {
                        command: Some("top".into()),
                        focus: true,
                        title: Some("monitor".into()),
                    }),
                    second: Box::new(LayoutNode::Pane {
                        command: Some("htop".into()),
                        focus: false,
                        title: None,
                    }),
                },
            }],
            env: vec![],
            pre_hook: Some("echo start".into()),
        };
        let json = serde_json::to_string_pretty(&original).unwrap();
        let parsed: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(original, parsed);
    }

    // ── Script generation ─────────────────────────────────────────────────────

    #[test]
    fn script_shebang() {
        let s = compile(&single_pane("s", "bash"));
        assert!(s.starts_with("#!/usr/bin/env bash"));
    }

    #[test]
    fn script_idempotency_guard() {
        let s = compile(&single_pane("s", "bash"));
        assert!(s.contains("tmux has-session"));
        assert!(s.contains("switch-client"));
        assert!(s.contains("attach-session"));
    }

    #[test]
    fn script_tmux_env_detection() {
        let s = compile(&single_pane("s", "bash"));
        // Both the guard and the final attach must use the TMUX variable
        let count = s.matches("${TMUX:-}").count();
        assert_eq!(count, 2, "TMUX detection should appear in guard and final attach");
    }

    #[test]
    fn script_new_session_command() {
        let s = compile(&single_pane("s", "bash"));
        assert!(s.contains("tmux new-session"));
        assert!(s.contains("'s'"), "session name should be single-quoted");
    }

    #[test]
    fn script_sends_command() {
        let s = compile(&single_pane("s", "vim"));
        assert!(s.contains("tmux send-keys"));
        assert!(s.contains("'vim'"));
    }

    #[test]
    fn script_focus_pane() {
        let s = compile(&single_pane("s", "bash"));
        assert!(s.contains("tmux select-pane -t \"${PANE_0}\""));
    }

    #[test]
    fn script_horizontal_split_40pct() {
        let session = Session {
            name: "s".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                layout: LayoutNode::Split {
                    direction: Direction::Horizontal,
                    ratio: 0.6,
                    first: Box::new(LayoutNode::Pane { command: None, focus: false, title: None }),
                    second: Box::new(LayoutNode::Pane { command: None, focus: false, title: None }),
                },
            }],
            env: vec![],
            pre_hook: None,
        };
        let s = compile(&session);
        assert!(s.contains("-h"), "horizontal split");
        assert!(s.contains("40%"), "second pane gets 40% (=100-60)");
    }

    #[test]
    fn script_vertical_split_70pct() {
        let session = Session {
            name: "s".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                layout: LayoutNode::Split {
                    direction: Direction::Vertical,
                    ratio: 0.3,
                    first: Box::new(LayoutNode::Pane { command: None, focus: false, title: None }),
                    second: Box::new(LayoutNode::Pane { command: None, focus: false, title: None }),
                },
            }],
            env: vec![],
            pre_hook: None,
        };
        let s = compile(&session);
        assert!(s.contains("-v"), "vertical split");
        assert!(s.contains("70%"), "second pane gets 70% (=100-30)");
    }

    #[test]
    fn script_three_pane_nested() {
        let raw = r#"{
          "name": "dev", "root": "/p",
          "windows": [{"name": "main", "layout": {
            "type": "split", "direction": "horizontal", "ratio": 0.6,
            "first":  {"type": "pane", "command": "emacs", "focus": true},
            "second": {
              "type": "split", "direction": "vertical", "ratio": 0.5,
              "first":  {"type": "pane", "command": "cargo watch"},
              "second": {"type": "pane", "command": "git log"}
            }
          }}]
        }"#;
        let session: Session = serde_json::from_str(raw).unwrap();
        let s = compile(&session);

        assert_eq!(s.matches("split-window").count(), 2, "3 panes = 2 splits");
        assert!(s.contains("'emacs'"));
        assert!(s.contains("'cargo watch'"));
        assert!(s.contains("'git log'"));
        assert!(s.contains("select-pane -t \"${PANE_0}\""), "emacs pane focused");
    }

    #[test]
    fn script_two_phase_ordering() {
        // All split-window calls must appear before any send-keys call
        let raw = r#"{
          "name": "s", "root": "/",
          "windows": [{"name": "w", "layout": {
            "type": "split", "direction": "horizontal", "ratio": 0.5,
            "first":  {"type": "pane", "command": "top"},
            "second": {"type": "pane", "command": "htop"}
          }}]
        }"#;
        let session: Session = serde_json::from_str(raw).unwrap();
        let s = compile(&session);

        let last_split = s.rfind("split-window").unwrap();
        let first_send = s.find("send-keys").unwrap();
        assert!(last_split < first_send, "all splits precede all send-keys");
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
                    env: vec![],
                    layout: LayoutNode::Pane { command: Some("vim".into()), focus: false, title: None },
                },
                Window {
                    name: "shell".into(),
                    root: None,
                    env: vec![],
                    layout: LayoutNode::Pane { command: Some("bash".into()), focus: false, title: None },
                },
            ],
            env: vec![],
            pre_hook: None,
        };
        let s = compile(&session);
        assert!(s.contains("tmux new-session"));
        assert!(s.contains("tmux new-window"));
        assert!(s.contains("'code'"));
        assert!(s.contains("'shell'"));
    }

    #[test]
    fn script_session_env_vars() {
        let session = Session {
            name: "e".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                layout: LayoutNode::Pane { command: None, focus: false, title: None },
            }],
            env: vec![
                EnvVar { key: "EDITOR".into(), value: "nvim".into() },
                EnvVar { key: "PAGER".into(), value: "less".into() },
            ],
            pre_hook: None,
        };
        let s = compile(&session);
        assert!(s.contains("export EDITOR='nvim'"));
        assert!(s.contains("export PAGER='less'"));
    }

    #[test]
    fn script_window_env_vars() {
        let session = Session {
            name: "we".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![EnvVar { key: "NODE_ENV".into(), value: "development".into() }],
                layout: LayoutNode::Pane { command: None, focus: false, title: None },
            }],
            env: vec![],
            pre_hook: None,
        };
        let s = compile(&session);
        assert!(s.contains("export NODE_ENV='development'"));
    }

    #[test]
    fn script_pane_title() {
        let session = Session {
            name: "t".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                layout: LayoutNode::Pane {
                    command: Some("top".into()),
                    focus: false,
                    title: Some("monitor".into()),
                },
            }],
            env: vec![],
            pre_hook: None,
        };
        let s = compile(&session);
        assert!(s.contains("select-pane -t \"${PANE_0}\" -T 'monitor'"));
    }

    #[test]
    fn script_pre_hook_before_new_session() {
        let session = Session {
            name: "ph".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                layout: LayoutNode::Pane { command: None, focus: false, title: None },
            }],
            env: vec![],
            pre_hook: Some("nix build".into()),
        };
        let s = compile(&session);
        let hook_pos = s.find("nix build").unwrap();
        let sess_pos = s.find("new-session").unwrap();
        assert!(hook_pos < sess_pos, "pre_hook runs before new-session");
    }

    #[test]
    fn script_window_root_overrides_session() {
        let session = Session {
            name: "wr".into(),
            root: Some("/session-root".into()),
            windows: vec![Window {
                name: "w".into(),
                root: Some("/window-root".into()),
                env: vec![],
                layout: LayoutNode::Pane { command: None, focus: false, title: None },
            }],
            env: vec![],
            pre_hook: None,
        };
        let s = compile(&session);
        assert!(s.contains("'/window-root'"), "window root takes precedence");
        assert!(!s.contains("'/session-root'"), "session root not used when window root is set");
    }
}
