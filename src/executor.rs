use crate::backend::TmuxBackend;
use crate::model::resolve_vars;
use crate::model::{Direction, LayoutNode, Session, WaitFor, Window};
use anyhow::Result;
use std::collections::HashMap;

// ─── Internal record ─────────────────────────────────────────────────────────

struct PaneRecord {
    id: String,
    command: Option<String>,
    title: Option<String>,
    focus: bool,
    wait_for: Option<WaitFor>,
}

// ─── Executor ────────────────────────────────────────────────────────────────

pub struct Executor<'a, B: TmuxBackend> {
    backend: &'a B,
}

impl<'a, B: TmuxBackend> Executor<'a, B> {
    pub fn new(backend: &'a B) -> Self {
        Self { backend }
    }

    pub fn run(&self, session: &Session) -> Result<()> {
        if self.backend.has_session(&session.name) {
            return self.backend.attach_or_switch(&session.name);
        }
        self.create_session(session)?;
        self.backend.select_window(&session.name, 0)?;
        self.backend.attach_or_switch(&session.name)
    }

    pub fn reload(&self, session: &Session) -> Result<()> {
        let _ = self.backend.kill_session(&session.name);
        self.create_session(session)?;
        self.backend.select_window(&session.name, 0)?;
        self.backend.attach_or_switch(&session.name)
    }

    fn create_session(&self, session: &Session) -> Result<()> {
        let vars = &session.vars;
        if let Some(hook) = &session.pre_hook {
            self.backend.run_command(&resolve_vars(hook, vars))?;
        }
        for (idx, window) in session.windows.iter().enumerate() {
            self.create_window(session, window, idx, vars)?;
        }
        Ok(())
    }

    fn create_window(
        &self,
        session: &Session,
        window: &Window,
        index: usize,
        vars: &HashMap<String, String>,
    ) -> Result<()> {
        let root = resolve_vars(
            window
                .root
                .as_deref()
                .or(session.root.as_deref())
                .unwrap_or("$HOME"),
            vars,
        );

        let initial = if index == 0 {
            let p = self
                .backend
                .new_session(&session.name, &root, &window.name)?;
            for (k, v) in &session.options {
                self.backend.set_option(&session.name, k, v)?;
            }
            p
        } else {
            self.backend
                .new_window(&session.name, &window.name, &root)?
        };

        // Phase 1: build pane structure
        let mut records = Vec::new();
        self.collect_structure(&window.layout, &initial, &root, vars, &mut records)?;

        // Phase 2: send commands / titles / wait_for
        let mut focus_pane: Option<String> = None;
        for rec in &records {
            if let Some(cmd) = &rec.command {
                self.backend.send_keys(&rec.id, cmd)?;
            }
            if let Some(wf) = &rec.wait_for {
                self.wait_for_pane_output(&rec.id, wf)?;
            }
            if let Some(title) = &rec.title {
                self.backend.set_pane_title(&rec.id, title)?;
            }
            if rec.focus {
                focus_pane = Some(rec.id.clone());
            }
        }

        for (k, v) in &window.options {
            self.backend
                .set_window_option(&session.name, &window.name, k, v)?;
        }
        if let Some(preset) = &window.select_layout {
            self.backend
                .select_layout(&session.name, &window.name, preset)?;
        }
        if let Some(fp) = focus_pane {
            self.backend.select_pane(&fp)?;
        }
        Ok(())
    }

    fn collect_structure(
        &self,
        node: &LayoutNode,
        current: &str,
        root: &str,
        vars: &HashMap<String, String>,
        records: &mut Vec<PaneRecord>,
    ) -> Result<()> {
        match node {
            LayoutNode::Pane {
                command,
                focus,
                title,
                wait_for,
            } => {
                records.push(PaneRecord {
                    id: current.to_string(),
                    command: command.as_ref().map(|c| resolve_vars(c, vars)),
                    title: title.as_ref().map(|t| resolve_vars(t, vars)),
                    focus: *focus,
                    wait_for: wait_for.clone(),
                });
            }
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let flag = match direction {
                    Direction::Horizontal => "-h",
                    Direction::Vertical => "-v",
                };
                let pct = ((1.0 - ratio).clamp(0.0, 1.0) * 100.0).round() as u32;
                let new_pane = self.backend.split_window(current, flag, pct, root)?;
                self.collect_structure(first, current, root, vars, records)?;
                self.collect_structure(second, &new_pane, root, vars, records)?;
            }
        }
        Ok(())
    }

    fn wait_for_pane_output(&self, pane_id: &str, wf: &WaitFor) -> Result<()> {
        for elapsed in 0..wf.timeout {
            let out = self.backend.capture_pane(pane_id)?;
            if out.contains(&wf.pattern) {
                return Ok(());
            }
            if elapsed + 1 < wf.timeout {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        anyhow::bail!(
            "timeout after {}s: {:?} not seen in pane {}",
            wf.timeout,
            wf.pattern,
            pane_id
        )
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::RecordingBackend;
    use crate::model::{EnvVar, LayoutNode, Session, WaitFor, Window};
    use std::collections::HashMap;

    fn simple_session(name: &str, cmd: Option<&str>) -> Session {
        Session {
            name: name.into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "main".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: cmd.map(|s| s.into()),
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        }
    }

    #[test]
    fn executor_single_pane_calls() {
        let b = RecordingBackend::new();
        let session = simple_session("mysession", Some("vim"));
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(calls.iter().any(|c| c.starts_with("has-session:")));
        assert!(calls.iter().any(|c| c.starts_with("new-session:")));
        assert!(calls
            .iter()
            .any(|c| c.starts_with("send-keys:") && c.contains("vim")));
        assert!(calls.iter().any(|c| c.starts_with("select-window:")));
        assert!(calls.iter().any(|c| c.starts_with("attach-or-switch:")));
    }

    #[test]
    fn executor_horizontal_split_order() {
        let b = RecordingBackend::new();
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Split {
                    direction: Direction::Horizontal,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane {
                        command: Some("top".into()),
                        focus: false,
                        title: None,
                        wait_for: None,
                    }),
                    second: Box::new(LayoutNode::Pane {
                        command: Some("htop".into()),
                        focus: false,
                        title: None,
                        wait_for: None,
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        let last_split = calls
            .iter()
            .rposition(|c| c.starts_with("split-window:"))
            .unwrap();
        let first_send = calls
            .iter()
            .position(|c| c.starts_with("send-keys:"))
            .unwrap();
        assert!(
            last_split < first_send,
            "all splits must precede all send-keys"
        );
    }

    #[test]
    fn executor_focus_pane_selected() {
        let b = RecordingBackend::new();
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Split {
                    direction: Direction::Horizontal,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane {
                        command: None,
                        focus: true,
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
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        // The first pane is the one with focus=true; it gets pane ID %0 (from new-session)
        assert!(
            calls.iter().any(|c| c == "select-pane:%0"),
            "select-pane should be called with the focused pane id"
        );
    }

    #[test]
    fn executor_session_exists_attaches() {
        let b = RecordingBackend::new();
        b.session_exists.set(true);
        let session = simple_session("exists", Some("vim"));
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(calls.iter().any(|c| c.starts_with("has-session:")));
        assert!(calls.iter().any(|c| c.starts_with("attach-or-switch:")));
        // No new-session or send-keys
        assert!(!calls.iter().any(|c| c.starts_with("new-session:")));
        assert!(!calls.iter().any(|c| c.starts_with("send-keys:")));
    }

    #[test]
    fn executor_reload_kills_then_creates() {
        let b = RecordingBackend::new();
        b.session_exists.set(true);
        let session = simple_session("s", Some("vim"));
        let ex = Executor::new(&b);
        ex.reload(&session).unwrap();

        let calls = b.calls();
        let kill_pos = calls
            .iter()
            .position(|c| c.starts_with("kill-session:"))
            .unwrap();
        let new_pos = calls
            .iter()
            .position(|c| c.starts_with("new-session:"))
            .unwrap();
        assert!(kill_pos < new_pos, "kill-session must precede new-session");
    }

    #[test]
    fn executor_wait_for_success() {
        let b = RecordingBackend::new();
        *b.capture_output.borrow_mut() = "Server ready on port 3000".to_string();

        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: Some("npm start".into()),
                    focus: false,
                    title: None,
                    wait_for: Some(WaitFor {
                        pattern: "ready".into(),
                        timeout: 5,
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        // Should succeed because capture_output contains "ready"
        assert!(ex.run(&session).is_ok());
    }

    #[test]
    fn executor_wait_for_timeout() {
        let b = RecordingBackend::new();
        *b.capture_output.borrow_mut() = "still starting...".to_string();

        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: Some("npm start".into()),
                    focus: false,
                    title: None,
                    wait_for: Some(WaitFor {
                        pattern: "ready".into(),
                        timeout: 1,
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        let result = ex.run(&session);
        assert!(result.is_err(), "should fail on timeout");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("timeout"), "error should mention timeout");
    }

    #[test]
    fn executor_session_options() {
        let b = RecordingBackend::new();
        let mut opts = HashMap::new();
        opts.insert("status".to_string(), "off".to_string());
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
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
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls.iter().any(|c| c == "set-option:s:status:off"),
            "set-option should be called for session options"
        );
    }

    #[test]
    fn executor_window_options() {
        let b = RecordingBackend::new();
        let mut wopts = HashMap::new();
        wopts.insert("synchronize-panes".to_string(), "on".to_string());
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
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
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls
                .iter()
                .any(|c| c == "set-window-option:s:w:synchronize-panes:on"),
            "set-window-option should be called for window options"
        );
    }

    #[test]
    fn executor_select_layout() {
        let b = RecordingBackend::new();
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: Some("tiled".into()),
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls.iter().any(|c| c == "select-layout:s:w:tiled"),
            "select-layout should be called when select_layout is set"
        );
    }

    #[test]
    fn executor_pane_title() {
        let b = RecordingBackend::new();
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: Some("my-title".into()),
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls.iter().any(|c| c == "set-pane-title:%0:my-title"),
            "set-pane-title should be called"
        );
    }

    #[test]
    fn executor_template_vars() {
        let b = RecordingBackend::new();
        let mut vars = HashMap::new();
        vars.insert("mydir".to_string(), "/home/user/project".to_string());
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: Some("cd {{mydir}}".into()),
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars,
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls
                .iter()
                .any(|c| c == "send-keys:%0:cd /home/user/project"),
            "template variable should be substituted in command"
        );
    }

    #[test]
    fn executor_cwd_template() {
        let b = RecordingBackend::new();
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: Some("cd {{cwd}}".into()),
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls.iter().any(|c| c == "send-keys:%0:cd $PWD"),
            "{{cwd}} should resolve to $PWD"
        );
    }

    #[test]
    fn executor_multiple_windows_uses_new_window() {
        let b = RecordingBackend::new();
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![
                Window {
                    name: "first".into(),
                    root: None,
                    env: vec![],
                    options: HashMap::new(),
                    select_layout: None,
                    layout: LayoutNode::Pane {
                        command: None,
                        focus: false,
                        title: None,
                        wait_for: None,
                    },
                },
                Window {
                    name: "second".into(),
                    root: None,
                    env: vec![],
                    options: HashMap::new(),
                    select_layout: None,
                    layout: LayoutNode::Pane {
                        command: Some("bash".into()),
                        focus: false,
                        title: None,
                        wait_for: None,
                    },
                },
            ],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(calls.iter().any(|c| c.starts_with("new-session:")));
        assert!(calls.iter().any(|c| c.starts_with("new-window:")));
    }

    #[test]
    fn executor_vertical_split() {
        let b = RecordingBackend::new();
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Split {
                    direction: Direction::Vertical,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane {
                        command: Some("top".into()),
                        focus: false,
                        title: None,
                        wait_for: None,
                    }),
                    second: Box::new(LayoutNode::Pane {
                        command: Some("htop".into()),
                        focus: false,
                        title: None,
                        wait_for: None,
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls.iter().any(|c| c.contains("-v")),
            "vertical split should use -v flag"
        );
    }

    #[test]
    fn executor_pre_hook_runs_before_session() {
        let b = RecordingBackend::new();
        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: Some("nix build".into()),
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        let hook_pos = calls
            .iter()
            .position(|c| c == "run-command:nix build")
            .unwrap();
        let sess_pos = calls
            .iter()
            .position(|c| c.starts_with("new-session:"))
            .unwrap();
        assert!(hook_pos < sess_pos, "pre_hook must run before new-session");
    }

    #[test]
    fn executor_window_root_overrides_session_root() {
        let b = RecordingBackend::new();
        let session = Session {
            name: "s".into(),
            root: Some("/session-root".into()),
            windows: vec![Window {
                name: "w".into(),
                root: Some("/window-root".into()),
                env: vec![],
                options: HashMap::new(),
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
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls.iter().any(|c| c.contains("/window-root")),
            "window root should be used"
        );
        assert!(
            !calls.iter().any(|c| c.contains("/session-root")),
            "session root should not be used when window root is set"
        );
    }

    #[test]
    fn executor_no_root_defaults_to_home() {
        let b = RecordingBackend::new();
        let session = Session {
            name: "s".into(),
            root: None,
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
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
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls.iter().any(|c| c.contains("$HOME")),
            "null root should fall back to $HOME"
        );
    }

    #[test]
    fn executor_wait_for_checks_multiple_iterations() {
        // timeout=2, pattern never found → should error after 2 iterations
        // This exercises the elapsed+1 < timeout sleep branch (but RecordingBackend
        // never sleeps, so the loop terminates immediately).
        let b = RecordingBackend::new();
        *b.capture_output.borrow_mut() = "not what we want".into();

        let session = Session {
            name: "s".into(),
            root: Some("/tmp".into()),
            windows: vec![Window {
                name: "w".into(),
                root: None,
                env: vec![],
                options: HashMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: None,
                    focus: false,
                    title: None,
                    wait_for: Some(WaitFor {
                        pattern: "ready".into(),
                        timeout: 2,
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: HashMap::new(),
            vars: HashMap::new(),
        };
        let ex = Executor::new(&b);
        let result = ex.run(&session);
        assert!(result.is_err());
        // Two capture-pane calls should have been recorded (one per iteration)
        let capture_count = b
            .calls()
            .iter()
            .filter(|c| c.starts_with("capture-pane:"))
            .count();
        assert_eq!(
            capture_count, 2,
            "should attempt exactly timeout iterations"
        );
    }

    // Suppress unused import warning for EnvVar in this test module
    #[allow(dead_code)]
    fn _use_env_var() -> EnvVar {
        EnvVar {
            key: "K".into(),
            value: "V".into(),
        }
    }
}
