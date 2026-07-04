use crate::model::{
    resolve_vars, shell_quote, shell_quote_template_vars, EnvVar, LayoutNode, PaneCommand,
    PaneTitle, Session, ShellCommand, ShellWord, TemplateVars, WaitPattern, WaitTimeoutSeconds,
    Window,
};
use anyhow::Result;

// ─── Internal record ─────────────────────────────────────────────────────────

/// Metadata collected for each leaf pane during the structure-building phase.
struct PaneRecord {
    var: String,
    command: Option<PaneCommand>,
    title: Option<PaneTitle>,
    focus: bool,
    has_wait_for: bool,
    wait_pattern: Option<WaitPattern>,
    wait_timeout: Option<WaitTimeoutSeconds>,
}

struct CompileWindowContext<'a> {
    root: &'a ShellWord,
    vars: &'a TemplateVars,
    tmux_env: &'a str,
}

// ─── Compiler ─────────────────────────────────────────────────────────────────

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

    pub fn compile(&mut self, session: &Session) -> Result<()> {
        self.lines.clear();
        self.pane_counter = 0;
        session.validate()?;
        // Check whether any pane uses wait_for so we can emit the helper
        let needs_wait_helper = self.session_uses_wait_for(session);

        self.emit_preamble(session, needs_wait_helper)?;
        for (idx, window) in session.windows.iter().enumerate() {
            self.compile_window(window, idx, session)?;
        }
        self.emit("tmux select-window -t \"$SESSION:^\"");
        self.emit_attach_or_switch();
        Ok(())
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

    fn session_uses_wait_for(&self, session: &Session) -> bool {
        session
            .windows
            .iter()
            .any(|w| self.layout_uses_wait_for(&w.layout))
    }

    fn layout_uses_wait_for(&self, node: &LayoutNode) -> bool {
        match node {
            LayoutNode::Pane { wait_for, .. } => wait_for.is_some(),
            LayoutNode::Split { first, second, .. } => {
                self.layout_uses_wait_for(first) || self.layout_uses_wait_for(second)
            }
        }
    }

    fn emit_wait_helper(&mut self) {
        self.emit("# wait for pattern in pane output");
        self.emit("_ntd_wait_pane() {");
        self.emit("  local pane=$1 pattern=$2 timeout=${3:-30} elapsed=0");
        self.emit("  while [ $elapsed -lt $timeout ]; do");
        self.emit("    if tmux capture-pane -t \"$pane\" -p | grep -qF -- \"$pattern\"; then");
        self.emit("      return 0");
        self.emit("    fi");
        self.emit("    sleep 1");
        self.emit("    elapsed=$((elapsed + 1))");
        self.emit("  done");
        self.emit("  echo \"timeout after ${timeout}s: '$pattern' not seen in pane $pane\" >&2");
        self.emit("  return 1");
        self.emit("}");
        self.emit("");
    }

    fn emit_preamble(&mut self, session: &Session, needs_wait_helper: bool) -> Result<()> {
        self.emit("#!/usr/bin/env bash");
        self.emit("set -euo pipefail");
        self.emit("");
        self.emit(format!("SESSION={}", shell_quote(session.name.as_str())));
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

        if needs_wait_helper {
            self.emit_wait_helper();
        }

        if let Some(hook) = &session.pre_hook {
            self.emit("# pre_hook");
            let hook = ShellCommand::new(resolve_vars(hook.as_str(), &session.vars))?;
            self.emit(hook.as_str());
            self.emit("");
        }
        Ok(())
    }

    /// Emits the final attach/switch block, handling nested-tmux correctly.
    fn emit_attach_or_switch(&mut self) {
        self.emit("if [ -n \"${TMUX:-}\" ]; then");
        self.emit("  exec tmux switch-client -t \"$SESSION\"");
        self.emit("else");
        self.emit("  exec tmux attach-session -t \"$SESSION\"");
        self.emit("fi");
    }

    fn compile_window(&mut self, window: &Window, index: usize, session: &Session) -> Result<()> {
        let root = shell_quote_template_vars(
            window
                .root
                .as_ref()
                .map(|root| root.as_str())
                .or_else(|| session.root.as_ref().map(|root| root.as_str()))
                .unwrap_or("~"),
            &session.vars,
        )?;

        self.emit(format!("# ── window {}: {} ──", index, window.name));

        let mut window_env = session.env.clone();
        window_env.extend(window.env.clone());
        let tmux_env = tmux_env_args(&window_env);

        // ── Phase 1: build pane structure ────────────────────────────────────
        let initial = if index == 0 {
            let var = self.alloc_pane();
            self.emit(format!(
                "{}=$(tmux new-session -d -s \"$SESSION\" -c {} -n {}{} -P -F '#{{pane_id}}')",
                var,
                root.as_str(),
                shell_quote(window.name.as_str()),
                tmux_env,
            ));
            // BTreeMap keeps generated scripts deterministic.
            for (k, v) in &session.options {
                self.emit(format!(
                    "tmux set-option -t \"$SESSION\" -- {} {}",
                    shell_quote(k.as_str()),
                    shell_quote(v.as_str()),
                ));
            }
            var
        } else {
            let var = self.alloc_pane();
            self.emit(format!(
                "{}=$(tmux new-window -t \"$SESSION\" -c {} -n {}{} -P -F '#{{pane_id}}')",
                var,
                root.as_str(),
                shell_quote(window.name.as_str()),
                tmux_env,
            ));
            var
        };

        let mut records: Vec<PaneRecord> = Vec::new();
        let ctx = CompileWindowContext {
            root: &root,
            vars: &session.vars,
            tmux_env: &tmux_env,
        };
        self.collect_structure(&window.layout, &initial, &ctx, &mut records, 0)?;

        // ── Phase 2: send commands and configure panes ───────────────────────
        let mut focus_var: Option<String> = None;
        for rec in &records {
            if let Some(cmd) = &rec.command {
                self.emit(format!(
                    "tmux send-keys -t \"${{{}}}\" -l -- {}",
                    rec.var,
                    shell_quote(cmd.as_str()),
                ));
                self.emit(format!("tmux send-keys -t \"${{{}}}\" Enter", rec.var));
            }
            if rec.has_wait_for {
                let pattern = rec
                    .wait_pattern
                    .as_ref()
                    .map(WaitPattern::as_str)
                    .unwrap_or("");
                let timeout = rec.wait_timeout.unwrap_or_default();
                self.emit(format!(
                    "_ntd_wait_pane \"${{{}}}\" {} {}",
                    rec.var,
                    shell_quote(pattern),
                    timeout.as_secs(),
                ));
            }
            if let Some(title) = &rec.title {
                self.emit(format!(
                    "tmux select-pane -t \"${{{}}}\" -T {}",
                    rec.var,
                    shell_quote(title.as_str()),
                ));
            }
            if rec.focus {
                focus_var = Some(rec.var.clone());
            }
        }

        // Emit window options.
        //
        // The target is `"$SESSION:"<quoted name>`: the double quote is closed
        // before the shell-quoted window name so the quote characters do NOT
        // reach tmux literally. Writing `"$SESSION:<quoted>"` would embed the
        // single quotes inside the target (`session:'w'`), which tmux reads as a
        // nonexistent window and aborts the `set -euo pipefail` script.
        // BTreeMap keeps generated scripts deterministic.
        for (k, v) in &window.options {
            self.emit(format!(
                "tmux set-window-option -t \"$SESSION:\"{}  -- {} {}",
                shell_quote(window.name.as_str()),
                shell_quote(k.as_str()),
                shell_quote(v.as_str()),
            ));
        }

        // Emit select-layout if specified (same target-quoting rule as above).
        if let Some(preset) = &window.select_layout {
            self.emit(format!(
                "tmux select-layout -t \"$SESSION:\"{}  -- {}",
                shell_quote(window.name.as_str()),
                shell_quote(preset.as_str()),
            ));
        }

        if let Some(fp) = focus_var {
            self.emit(format!("tmux select-pane -t \"${{{}}}\"", fp));
        }

        self.emit("");
        Ok(())
    }

    /// Recursively emits `split-window` commands (phase 1) and appends a
    /// [`PaneRecord`] for every leaf pane encountered.
    fn collect_structure(
        &mut self,
        node: &LayoutNode,
        current: &str,
        ctx: &CompileWindowContext<'_>,
        records: &mut Vec<PaneRecord>,
        depth: usize,
    ) -> Result<()> {
        if depth > crate::MAX_LAYOUT_DEPTH {
            anyhow::bail!(
                "layout tree is too deeply nested (max depth: {})",
                crate::MAX_LAYOUT_DEPTH
            );
        }
        match node {
            LayoutNode::Pane {
                command,
                focus,
                title,
                wait_for,
            } => {
                records.push(PaneRecord {
                    var: current.to_string(),
                    command: command
                        .as_ref()
                        .map(|c| PaneCommand::new(resolve_vars(c.as_str(), ctx.vars)))
                        .transpose()?,
                    title: title
                        .as_ref()
                        .map(|t| PaneTitle::new(resolve_vars(t.as_str(), ctx.vars)))
                        .transpose()?,
                    focus: *focus,
                    has_wait_for: wait_for.is_some(),
                    wait_pattern: wait_for.as_ref().map(|wf| wf.pattern.clone()),
                    wait_timeout: wait_for.as_ref().map(|wf| wf.timeout),
                });
            }
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let new_pane = self.alloc_pane();
                let flag = direction.tmux_split_flag();
                // -l specifies the size of the *new* (second) pane
                let pct = ratio.tmux_second_pane_percent();
                self.emit(format!(
                    "{}=$(tmux split-window -t \"${{{}}}\" {} -l {} -c {}{} -P -F '#{{pane_id}}')",
                    new_pane,
                    current,
                    flag.as_str(),
                    pct.as_tmux_size(),
                    ctx.root.as_str(),
                    ctx.tmux_env,
                ));
                self.collect_structure(first, current, ctx, records, depth + 1)?;
                self.collect_structure(second, &new_pane, ctx, records, depth + 1)?;
            }
        }
        Ok(())
    }
}

fn tmux_env_args(env: &[EnvVar]) -> String {
    env.iter()
        .map(|ev| format!(" -e {}={}", ev.key, shell_quote(ev.value.as_str())))
        .collect()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Direction, EnvVar, EnvVarName, EnvVarValue, PaneCommand, PaneTitle, RootTemplate, Session,
        ShellCommand, SplitRatio, TemplateVarName, TemplateVarValue, TmuxLayoutPreset, TmuxName,
        TmuxOptionName, TmuxOptionValue, WaitPattern, WaitTimeoutSeconds, Window,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    fn ratio(value: f64) -> SplitRatio {
        SplitRatio::new(value).unwrap()
    }

    fn pc(value: &str) -> PaneCommand {
        PaneCommand::new(value).unwrap()
    }

    fn pt(value: &str) -> PaneTitle {
        PaneTitle::new(value).unwrap()
    }

    fn wp(value: &str) -> WaitPattern {
        WaitPattern::new(value).unwrap()
    }

    fn tmux_name(value: &str) -> TmuxName {
        TmuxName::new(value).unwrap()
    }

    fn root_template(value: &str) -> RootTemplate {
        RootTemplate::new(value).unwrap()
    }

    fn shell_command(value: &str) -> ShellCommand {
        ShellCommand::new(value).unwrap()
    }

    fn env_key(value: &str) -> EnvVarName {
        EnvVarName::new(value).unwrap()
    }

    fn env_value(value: &str) -> EnvVarValue {
        EnvVarValue::new(value).unwrap()
    }

    fn option_name(value: &str) -> TmuxOptionName {
        TmuxOptionName::new(value).unwrap()
    }

    fn option_value(value: &str) -> TmuxOptionValue {
        TmuxOptionValue::new(value).unwrap()
    }

    fn layout_preset(value: &str) -> TmuxLayoutPreset {
        TmuxLayoutPreset::new(value).unwrap()
    }

    fn var_key(value: &str) -> TemplateVarName {
        TemplateVarName::new(value).unwrap()
    }

    fn var_value(value: &str) -> TemplateVarValue {
        TemplateVarValue::new(value).unwrap()
    }

    fn compile(session: &Session) -> String {
        let mut c = Compiler::new();
        c.compile(session).unwrap();
        c.into_script()
    }

    fn line_pos(script: &str, needle: &str) -> usize {
        script
            .find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?} in script:\n{script}"))
    }

    fn single_pane(name: &str, cmd: &str) -> Session {
        Session {
            name: tmux_name(name),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("main"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: Some(pc(cmd)),
                    focus: true,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
        assert_eq!(
            node,
            LayoutNode::Pane {
                command: None,
                focus: false,
                title: None,
                wait_for: None
            }
        );
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
            LayoutNode::Pane {
                command: Some(pc("vim")),
                focus: true,
                title: Some(pt("editor")),
                wait_for: None,
            }
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
            LayoutNode::Split {
                direction, ratio, ..
            } => {
                assert_eq!(direction, Direction::Horizontal);
                assert!((ratio.get() - 0.6).abs() < f64::EPSILON);
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
            name: tmux_name("rt"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![EnvVar {
                    key: env_key("FOO"),
                    value: env_value("bar"),
                }],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Split {
                    direction: Direction::Vertical,
                    ratio: ratio(0.4),
                    first: Box::new(LayoutNode::Pane {
                        command: Some(pc("top")),
                        focus: true,
                        title: Some(pt("monitor")),
                        wait_for: None,
                    }),
                    second: Box::new(LayoutNode::Pane {
                        command: Some(pc("htop")),
                        focus: false,
                        title: None,
                        wait_for: None,
                    }),
                },
            }],
            env: vec![],
            pre_hook: Some(shell_command("echo start")),
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
        assert_eq!(
            count, 2,
            "TMUX detection should appear in guard and final attach"
        );
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
        assert!(s.contains("tmux send-keys -t \"${PANE_0}\" -l -- 'vim'"));
        assert!(s.contains("tmux send-keys -t \"${PANE_0}\" Enter"));
    }

    #[test]
    fn script_sends_literal_command_before_enter() {
        let s = compile(&single_pane("s", "printf Enter -n"));
        assert!(s.contains("tmux send-keys -t \"${PANE_0}\" -l -- 'printf Enter -n'"));
        assert!(s.contains("tmux send-keys -t \"${PANE_0}\" Enter"));
        assert!(
            !s.contains("'printf Enter -n' Enter"),
            "command text must not be mixed with tmux key names"
        );
    }

    #[test]
    fn script_focus_pane() {
        let s = compile(&single_pane("s", "bash"));
        assert!(s.contains("tmux select-pane -t \"${PANE_0}\""));
    }

    #[test]
    fn script_horizontal_split_40pct() {
        let session = Session {
            name: tmux_name("s"),
            root: None,
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Split {
                    direction: Direction::Horizontal,
                    ratio: ratio(0.6),
                    first: Box::new(LayoutNode::Pane {
                        command: None,
                        focus: false,
                        title: None,
                        wait_for: None,
                    }),
                    second: Box::new(LayoutNode::Pane {
                        command: None,
                        focus: false,
                        title: None,
                        wait_for: None,
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(s.contains("-h"), "horizontal split");
        assert!(s.contains("40%"), "second pane gets 40% (=100-60)");
    }

    #[test]
    fn script_vertical_split_70pct() {
        let session = Session {
            name: tmux_name("s"),
            root: None,
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Split {
                    direction: Direction::Vertical,
                    ratio: ratio(0.3),
                    first: Box::new(LayoutNode::Pane {
                        command: None,
                        focus: false,
                        title: None,
                        wait_for: None,
                    }),
                    second: Box::new(LayoutNode::Pane {
                        command: None,
                        focus: false,
                        title: None,
                        wait_for: None,
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
        assert!(
            s.contains("select-pane -t \"${PANE_0}\""),
            "emacs pane focused"
        );
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
            name: tmux_name("multi"),
            root: Some(root_template("/tmp")),
            windows: vec![
                Window {
                    name: tmux_name("code"),
                    root: None,
                    env: vec![],
                    options: BTreeMap::new(),
                    select_layout: None,
                    layout: LayoutNode::Pane {
                        command: Some(pc("vim")),
                        focus: false,
                        title: None,
                        wait_for: None,
                    },
                },
                Window {
                    name: tmux_name("shell"),
                    root: None,
                    env: vec![],
                    options: BTreeMap::new(),
                    select_layout: None,
                    layout: LayoutNode::Pane {
                        command: Some(pc("bash")),
                        focus: false,
                        title: None,
                        wait_for: None,
                    },
                },
            ],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
            name: tmux_name("e"),
            root: None,
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![
                EnvVar {
                    key: env_key("EDITOR"),
                    value: env_value("nvim"),
                },
                EnvVar {
                    key: env_key("PAGER"),
                    value: env_value("less"),
                },
            ],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(s.contains("-e EDITOR='nvim'"));
        assert!(s.contains("-e PAGER='less'"));
        assert!(!s.contains("export EDITOR="));
        assert!(!s.contains("export PAGER="));
    }

    #[test]
    fn script_window_env_vars() {
        let session = Session {
            name: tmux_name("we"),
            root: None,
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![EnvVar {
                    key: env_key("NODE_ENV"),
                    value: env_value("development"),
                }],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(s.contains("-e NODE_ENV='development'"));
        assert!(!s.contains("export NODE_ENV="));
    }

    #[test]
    fn script_window_env_does_not_leak_to_next_window() {
        let session = Session {
            name: tmux_name("we"),
            root: None,
            windows: vec![
                Window {
                    name: tmux_name("app"),
                    root: None,
                    env: vec![EnvVar {
                        key: env_key("NODE_ENV"),
                        value: env_value("development"),
                    }],
                    options: BTreeMap::new(),
                    select_layout: None,
                    layout: LayoutNode::pane(),
                },
                Window {
                    name: tmux_name("shell"),
                    root: None,
                    env: vec![],
                    options: BTreeMap::new(),
                    select_layout: None,
                    layout: LayoutNode::pane(),
                },
            ],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        let first = s
            .lines()
            .find(|line| line.contains("tmux new-session"))
            .unwrap();
        let second = s
            .lines()
            .find(|line| line.contains("tmux new-window"))
            .unwrap();
        assert!(first.contains("-e NODE_ENV='development'"));
        assert!(!second.contains("NODE_ENV"));
    }

    #[test]
    fn script_pane_title() {
        let session = Session {
            name: tmux_name("t"),
            root: None,
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: Some(pc("top")),
                    focus: false,
                    title: Some(pt("monitor")),
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(s.contains("select-pane -t \"${PANE_0}\" -T 'monitor'"));
    }

    #[test]
    fn script_pre_hook_before_new_session() {
        let session = Session {
            name: tmux_name("ph"),
            root: None,
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: Some(shell_command("nix build")),
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        let hook_pos = s.find("nix build").unwrap();
        let sess_pos = s.find("new-session").unwrap();
        assert!(hook_pos < sess_pos, "pre_hook runs before new-session");
    }

    #[test]
    fn script_window_root_overrides_session() {
        let session = Session {
            name: tmux_name("wr"),
            root: Some(root_template("/session-root")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: Some(root_template("/window-root")),
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(s.contains("'/window-root'"), "window root takes precedence");
        assert!(
            !s.contains("'/session-root'"),
            "session root not used when window root is set"
        );
    }

    #[test]
    fn script_root_builtin_cwd_remains_shell_expression() {
        let mut session = single_pane("root-builtins", "pwd");
        session.root = Some(root_template("{{cwd}}/project"));

        let script = compile(&session);

        assert!(
            script.contains("-c \"${PWD}\"'/project'"),
            "root builtin should remain a quoted shell expression in scripts: {script}"
        );
        assert!(
            !script.contains("'$PWD/project'"),
            "root builtin must not be quoted as a literal string: {script}"
        );
    }

    // ── New feature tests ─────────────────────────────────────────────────────

    #[test]
    fn compiler_options_session() {
        let mut opts = BTreeMap::new();
        opts.insert(option_name("status"), option_value("off"));
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: opts,
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(
            s.contains("tmux set-option"),
            "set-option should appear in script"
        );
        assert!(s.contains("'status'"), "option key should be quoted");
        assert!(s.contains("'off'"), "option value should be quoted");
    }

    #[test]
    fn compiler_emits_session_options_in_key_order() {
        let mut opts = BTreeMap::new();
        opts.insert(option_name("status-right"), option_value("right"));
        opts.insert(option_name("base-index"), option_value("1"));
        opts.insert(option_name("status-left"), option_value("left"));
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: opts,
            vars: BTreeMap::new(),
        };
        let script = compile(&session);
        let base = line_pos(
            &script,
            "tmux set-option -t \"$SESSION\" -- 'base-index' '1'",
        );
        let left = line_pos(
            &script,
            "tmux set-option -t \"$SESSION\" -- 'status-left' 'left'",
        );
        let right = line_pos(
            &script,
            "tmux set-option -t \"$SESSION\" -- 'status-right' 'right'",
        );
        assert!(
            base < left && left < right,
            "session options should follow key order:\n{script}"
        );
    }

    #[test]
    fn compiler_options_window() {
        let mut wopts = BTreeMap::new();
        wopts.insert(option_name("synchronize-panes"), option_value("on"));
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: wopts,
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(
            s.contains("tmux set-window-option"),
            "set-window-option should appear"
        );
        assert!(s.contains("'synchronize-panes'"), "option key quoted");
        assert!(s.contains("'on'"), "option value quoted");
    }

    #[test]
    fn compiler_emits_window_options_in_key_order() {
        let mut wopts = BTreeMap::new();
        wopts.insert(option_name("synchronize-panes"), option_value("on"));
        wopts.insert(option_name("automatic-rename"), option_value("off"));
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: wopts,
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let script = compile(&session);
        let automatic = line_pos(
            &script,
            "tmux set-window-option -t \"$SESSION:\"'w'  -- 'automatic-rename' 'off'",
        );
        let synchronize = line_pos(
            &script,
            "tmux set-window-option -t \"$SESSION:\"'w'  -- 'synchronize-panes' 'on'",
        );
        assert!(
            automatic < synchronize,
            "window options should follow key order:\n{script}"
        );
    }

    #[test]
    fn compiler_select_layout() {
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: Some(layout_preset("tiled")),
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(
            s.contains("tmux select-layout"),
            "select-layout should appear"
        );
        assert!(s.contains("'tiled'"), "layout preset quoted");
    }

    #[test]
    fn compiler_window_target_quoting_is_well_formed() {
        // Regression: the window name must be single-quoted OUTSIDE the
        // double-quoted `$SESSION:` prefix, so the target expands to `s:w`, not
        // the literally-quoted `s:'w'` (which tmux rejects, aborting `set -e`).
        let mut wopts = BTreeMap::new();
        wopts.insert(option_name("synchronize-panes"), option_value("on"));
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: wopts,
                select_layout: Some(layout_preset("tiled")),
                layout: LayoutNode::pane(),
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(
            s.contains("set-window-option -t \"$SESSION:\"'w'"),
            "window-option target must be \"$SESSION:\"'w':\n{s}"
        );
        assert!(
            s.contains("select-layout -t \"$SESSION:\"'w'"),
            "select-layout target must be \"$SESSION:\"'w':\n{s}"
        );
        assert!(
            !s.contains("\"$SESSION:'w'\""),
            "the window name must not be quoted INSIDE the target:\n{s}"
        );
    }

    #[test]
    fn compiler_wait_for_helper() {
        use crate::model::WaitFor;
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: Some(pc("npm start")),
                    focus: false,
                    title: None,
                    wait_for: Some(WaitFor {
                        pattern: wp("ready"),
                        timeout: WaitTimeoutSeconds::new(30).unwrap(),
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(
            s.contains("_ntd_wait_pane"),
            "_ntd_wait_pane helper should be emitted"
        );
    }

    #[test]
    fn compiler_no_wait_helper_if_unused() {
        let s = compile(&single_pane("s", "bash"));
        assert!(
            !s.contains("_ntd_wait_pane"),
            "_ntd_wait_pane helper should NOT be emitted when no wait_for"
        );
    }

    #[test]
    fn compiler_wait_for_in_nested_split() {
        // wait_for lives in the SECOND child of a split, exercising the recursive
        // layout_uses_wait_for(second) path in session_uses_wait_for.
        use crate::model::WaitFor;
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Split {
                    direction: Direction::Horizontal,
                    ratio: ratio(0.5),
                    first: Box::new(LayoutNode::Pane {
                        command: None,
                        focus: false,
                        title: None,
                        wait_for: None,
                    }),
                    second: Box::new(LayoutNode::Pane {
                        command: Some(pc("npm start")),
                        focus: false,
                        title: None,
                        wait_for: Some(WaitFor {
                            pattern: wp("listening"),
                            timeout: WaitTimeoutSeconds::new(5).unwrap(),
                        }),
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(
            s.contains("_ntd_wait_pane"),
            "_ntd_wait_pane must be emitted when wait_for is nested in second child"
        );
        assert!(s.contains("'listening'"));
    }

    #[test]
    fn compiler_template_var() {
        let mut vars = BTreeMap::new();
        vars.insert(var_key("mydir"), var_value("/home/user/project"));
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: Some(pc("cd {{mydir}}")),
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars,
        };
        let s = compile(&session);
        assert!(
            s.contains("cd /home/user/project"),
            "template variable should be substituted"
        );
    }

    // ── Security: grep -qF ───────────────────────────────────────────────────

    #[test]
    fn compiler_wait_helper_uses_grep_fixed_string() {
        use crate::model::WaitFor;
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: Some(WaitFor {
                        pattern: wp("ready"),
                        timeout: WaitTimeoutSeconds::new(5).unwrap(),
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let s = compile(&session);
        assert!(
            s.contains("grep -qF -- \"$pattern\""),
            "wait helper must use grep -qF -- (fixed string) so leading '-' is data"
        );
        assert!(
            !s.contains("grep -q \""),
            "grep -q with unquoted pattern is absent"
        );
        assert!(
            !s.contains("grep -qF \"$pattern\""),
            "grep must not parse a leading '-' pattern as an option"
        );
    }

    #[test]
    fn compiler_compile_resets_reusable_state() {
        let mut compiler = Compiler::new();
        compiler.compile(&single_pane("first", "vim")).unwrap();

        compiler.compile(&single_pane("second", "bash")).unwrap();
        let s = compiler.into_script();

        assert!(s.contains("SESSION='second'"));
        assert!(s.contains("tmux send-keys -t \"${PANE_0}\" -l -- 'bash'"));
        assert!(!s.contains("SESSION='first'"));
        assert!(!s.contains("'vim'"));
        assert!(!s.contains("PANE_1"));
    }

    // ── Security: recursion depth limit ──────────────────────────────────────

    #[test]
    fn compiler_depth_limit_rejects_oversized_tree() {
        use crate::test_fixtures::make_deeply_nested;
        let session = Session {
            name: tmux_name("s"),
            root: None,
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: make_deeply_nested(65),
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let mut c = Compiler::new();
        let result = c.compile(&session);
        assert!(result.is_err(), "should fail for depth > MAX_LAYOUT_DEPTH");
        assert!(
            result.unwrap_err().to_string().contains("deeply nested"),
            "error should mention nesting"
        );
    }

    #[test]
    fn compiler_depth_limit_accepts_depth_64() {
        use crate::test_fixtures::make_deeply_nested;
        let session = Session {
            name: tmux_name("s"),
            root: None,
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: make_deeply_nested(64),
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let mut c = Compiler::new();
        assert!(c.compile(&session).is_ok(), "depth 64 should be accepted");
    }
}
