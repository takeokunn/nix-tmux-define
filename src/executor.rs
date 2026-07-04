use crate::backend::TmuxBackend;
use crate::layout_plan::LayoutPlan;
use crate::model::{resolve_tmux_arg_vars, resolve_vars};
use crate::model::{
    EnvVar, PaneId, Session, ShellCommand, TemplateVars, TmuxName, WaitFor, Window,
};
use anyhow::{Context, Error, Result};

// ─── Executor ────────────────────────────────────────────────────────────────

pub struct Executor<'a, B: TmuxBackend> {
    backend: &'a B,
}

impl<'a, B: TmuxBackend> Executor<'a, B> {
    pub fn new(backend: &'a B) -> Self {
        Self { backend }
    }

    pub fn run(&self, session: &Session) -> Result<()> {
        session.validate()?;
        let session_name = session.name.clone();
        if self.backend.has_session(&session_name) {
            return self.backend.attach_or_switch(&session_name);
        }
        self.create_session(session, &session_name)?;
        if let Some(first) = session.windows.first() {
            self.backend.select_window(&session_name, &first.name)?;
        }
        self.backend.attach_or_switch(&session_name)
    }

    pub fn reload(&self, session: &Session) -> Result<()> {
        session.validate()?;
        let session_name = session.name.clone();
        if self.backend.has_session(&session_name) {
            self.replace_existing_session(session, &session_name)?;
        } else {
            self.create_session(session, &session_name)?;
        }
        if let Some(first) = session.windows.first() {
            self.backend.select_window(&session_name, &first.name)?;
        }
        self.backend.attach_or_switch(&session_name)
    }

    fn replace_existing_session(&self, session: &Session, session_name: &TmuxName) -> Result<()> {
        let replacement_tmux_name =
            reload_side_session_name(session_name, ReloadSideSession::Replacement)?;
        let backup_tmux_name = reload_side_session_name(session_name, ReloadSideSession::Backup)?;
        let mut replacement = session.clone();
        replacement.name = replacement_tmux_name.clone();

        self.create_session(&replacement, &replacement_tmux_name)
            .with_context(|| {
                format!(
                    "failed to build replacement tmux session {:?}",
                    replacement_tmux_name.as_str()
                )
            })?;

        if let Err(err) = self.backend.rename_session(session_name, &backup_tmux_name) {
            let _ = self.backend.kill_session(&replacement_tmux_name);
            return Err(err.context(format!(
                "failed to move existing tmux session {:?} aside",
                session.name
            )));
        }

        if let Err(err) = self
            .backend
            .rename_session(&replacement_tmux_name, session_name)
        {
            let restore_result = self.backend.rename_session(&backup_tmux_name, session_name);
            let cleanup_result = self.backend.kill_session(&replacement_tmux_name);
            let mut message = format!(
                "failed to promote replacement tmux session {:?} to {:?}",
                replacement_tmux_name.as_str(),
                session.name.as_str()
            );
            if let Err(restore_err) = restore_result {
                message.push_str(&format!(
                    "; also failed to restore original session from {:?}: {restore_err}",
                    backup_tmux_name.as_str()
                ));
            }
            if let Err(cleanup_err) = cleanup_result {
                message.push_str(&format!(
                    "; also failed to remove replacement session {:?}: {cleanup_err}",
                    replacement_tmux_name.as_str()
                ));
            }
            return Err(err.context(message));
        }

        self.backend
            .kill_session(&backup_tmux_name)
            .with_context(|| {
                format!(
                    "replacement tmux session {:?} is active, but failed to remove backup session {:?}",
                    session.name.as_str(),
                    backup_tmux_name.as_str()
                )
            })?;
        Ok(())
    }

    fn create_session(&self, session: &Session, session_name: &TmuxName) -> Result<()> {
        let vars = &session.vars;
        if let Some(hook) = &session.pre_hook {
            let command = ShellCommand::new(resolve_vars(hook.as_str(), vars))?;
            self.backend.run_command(&command)?;
        }
        for (idx, window) in session.windows.iter().enumerate() {
            if let Err(err) = self.create_window(session, session_name, window, idx, vars) {
                if idx > 0 {
                    return Err(self.rollback_session(session_name, err));
                }
                return Err(err);
            }
        }
        Ok(())
    }

    fn rollback_session(&self, session_name: &TmuxName, cause: Error) -> Error {
        let cause_msg = cause.to_string();
        match self.backend.kill_session(session_name) {
            Ok(()) => cause.context(format!(
                "rolled back partially created tmux session {:?} after error: {cause_msg}",
                session_name.as_str()
            )),
            Err(rollback_err) => cause.context(format!(
                "failed to roll back partially created tmux session {:?} after error: {cause_msg}: {rollback_err}",
                session_name.as_str()
            )),
        }
    }

    fn create_window(
        &self,
        session: &Session,
        session_name: &TmuxName,
        window: &Window,
        index: usize,
        vars: &TemplateVars,
    ) -> Result<()> {
        let root = resolve_tmux_arg_vars(
            window
                .root
                .as_ref()
                .map(|root| root.as_str())
                .or_else(|| session.root.as_ref().map(|root| root.as_str()))
                .unwrap_or("~"),
            vars,
        )?;
        let env = effective_window_env(session, window);
        let window_name = window.name.clone();

        // Single source of truth for pane structure, shared with the compiler.
        let plan = LayoutPlan::build(&window.layout, vars)?;

        let initial = if index == 0 {
            self.backend
                .new_session_with_env(session_name, &root, &window_name, &env)?
        } else {
            self.backend
                .new_window_with_env(session_name, &window_name, &root, &env)?
        };

        let configure_result = (|| -> Result<()> {
            if index == 0 {
                for (k, v) in &session.options {
                    self.backend.set_option(session_name, k, v)?;
                }
            }

            // Phase 1: build pane structure. `pane_ids[i]` is the tmux pane for
            // plan pane index `i`; splits are ordered so a parent always exists
            // before it is split (parent < new, new increases by one each step).
            let mut pane_ids: Vec<PaneId> = Vec::with_capacity(plan.pane_count());
            pane_ids.push(initial.clone());
            for split in plan.splits() {
                debug_assert_eq!(split.new, pane_ids.len(), "plan pane indices must be dense");
                let new_pane = self.backend.split_window_with_env(
                    &pane_ids[split.parent],
                    split.flag,
                    split.pct,
                    &root,
                    &env,
                )?;
                pane_ids.push(new_pane);
            }

            // Phase 2: send commands / titles / wait_for
            let mut focus_pane: Option<PaneId> = None;
            for leaf in plan.leaves() {
                let pane = &pane_ids[leaf.pane];
                if let Some(cmd) = &leaf.command {
                    self.backend.send_keys(pane, cmd)?;
                }
                if let Some(wf) = &leaf.wait_for {
                    self.wait_for_pane_output(pane, wf)?;
                }
                if let Some(title) = &leaf.title {
                    self.backend.set_pane_title(pane, title)?;
                }
                if leaf.focus {
                    focus_pane = Some(pane.clone());
                }
            }

            for (k, v) in &window.options {
                self.backend
                    .set_window_option(session_name, &window_name, k, v)?;
            }
            if let Some(preset) = &window.select_layout {
                self.backend
                    .select_layout(session_name, &window_name, preset)?;
            }
            if let Some(fp) = focus_pane {
                self.backend.select_pane(&fp)?;
            }
            Ok(())
        })();

        match configure_result {
            Ok(()) => Ok(()),
            Err(err) if index == 0 => Err(self.rollback_session(session_name, err)),
            Err(err) => Err(err),
        }
    }

    fn wait_for_pane_output(&self, pane_id: &PaneId, wf: &WaitFor) -> Result<()> {
        // Matches the bash helper: check, sleep 1s, repeat up to timeout times.
        // Each iteration is "check then wait", so timeout=N allows N full seconds.
        let timeout = wf.timeout.as_secs();
        for _ in 0..timeout {
            let out = self.backend.capture_pane(pane_id)?;
            if out.contains(wf.pattern.as_str()) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        anyhow::bail!(
            "timeout after {}s: {:?} not seen in pane {}",
            timeout,
            wf.pattern.as_str(),
            pane_id.as_str()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReloadSideSession {
    Replacement,
    Backup,
}

impl ReloadSideSession {
    fn suffix(self) -> &'static str {
        match self {
            Self::Replacement => "new",
            Self::Backup => "old",
        }
    }
}

fn reload_side_session_name(session_name: &TmuxName, side: ReloadSideSession) -> Result<TmuxName> {
    TmuxName::new(format!(
        "{}__ntd_reload_{}_{}",
        session_name.as_str(),
        side.suffix(),
        std::process::id()
    ))
    .with_context(|| {
        format!(
            "failed to derive {:?} reload tmux session name from {:?}",
            side,
            session_name.as_str()
        )
    })
}

fn effective_window_env(session: &Session, window: &Window) -> Vec<EnvVar> {
    let mut env = session.env.clone();
    env.extend(window.env.clone());
    env
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{RecordingBackend, TmuxBackend};
    use crate::model::{
        Direction, EnvVar, EnvVarName, EnvVarValue, LayoutNode, PaneCommand, PaneTitle,
        ResolvedTmuxArg, RootTemplate, Session, ShellCommand, SplitRatio, TemplateVarName,
        TemplateVarValue, TmuxLayoutPreset, TmuxName, TmuxOptionName, TmuxOptionValue,
        TmuxPanePercent, TmuxSplitFlag, WaitFor, WaitPattern, WaitTimeoutSeconds, Window,
    };
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

    #[test]
    fn reload_side_session_name_returns_typed_replacement_name() {
        let name = reload_side_session_name(&tmux_name("demo"), ReloadSideSession::Replacement)
            .unwrap()
            .into_inner();

        assert!(name.starts_with("demo__ntd_reload_new_"), "{name}");
    }

    #[test]
    fn reload_side_session_name_returns_typed_backup_name() {
        let name = reload_side_session_name(&tmux_name("demo"), ReloadSideSession::Backup)
            .unwrap()
            .into_inner();

        assert!(name.starts_with("demo__ntd_reload_old_"), "{name}");
    }

    fn simple_session(name: &str, cmd: Option<&str>) -> Session {
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
                    command: cmd.map(pc),
                    focus: false,
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

    fn call_pos(calls: &[String], expected: &str) -> usize {
        calls
            .iter()
            .position(|call| call == expected)
            .unwrap_or_else(|| panic!("missing call {expected:?}: {calls:#?}"))
    }

    #[derive(Default)]
    struct FailingBackend {
        inner: RecordingBackend,
        fail_set_option: bool,
        fail_new_window: bool,
    }

    impl FailingBackend {
        fn calls(&self) -> Vec<String> {
            self.inner.calls()
        }
    }

    impl TmuxBackend for FailingBackend {
        fn has_session(&self, name: &TmuxName) -> bool {
            self.inner.has_session(name)
        }

        fn new_session(
            &self,
            name: &TmuxName,
            root: &ResolvedTmuxArg,
            window_name: &TmuxName,
        ) -> Result<PaneId> {
            self.inner.new_session(name, root, window_name)
        }

        fn new_session_with_env(
            &self,
            name: &TmuxName,
            root: &ResolvedTmuxArg,
            window_name: &TmuxName,
            env: &[EnvVar],
        ) -> Result<PaneId> {
            self.inner
                .new_session_with_env(name, root, window_name, env)
        }

        fn split_window(
            &self,
            pane_id: &PaneId,
            flag: TmuxSplitFlag,
            pct: TmuxPanePercent,
            root: &ResolvedTmuxArg,
        ) -> Result<PaneId> {
            self.inner.split_window(pane_id, flag, pct, root)
        }

        fn split_window_with_env(
            &self,
            pane_id: &PaneId,
            flag: TmuxSplitFlag,
            pct: TmuxPanePercent,
            root: &ResolvedTmuxArg,
            env: &[EnvVar],
        ) -> Result<PaneId> {
            self.inner
                .split_window_with_env(pane_id, flag, pct, root, env)
        }

        fn new_window(
            &self,
            session: &TmuxName,
            name: &TmuxName,
            root: &ResolvedTmuxArg,
        ) -> Result<PaneId> {
            self.inner.new_window(session, name, root)
        }

        fn new_window_with_env(
            &self,
            session: &TmuxName,
            name: &TmuxName,
            root: &ResolvedTmuxArg,
            env: &[EnvVar],
        ) -> Result<PaneId> {
            if self.fail_new_window {
                anyhow::bail!("new-window failed");
            }
            self.inner.new_window_with_env(session, name, root, env)
        }

        fn send_keys(&self, pane_id: &PaneId, keys: &PaneCommand) -> Result<()> {
            self.inner.send_keys(pane_id, keys)
        }

        fn select_pane(&self, pane_id: &PaneId) -> Result<()> {
            self.inner.select_pane(pane_id)
        }

        fn set_pane_title(&self, pane_id: &PaneId, title: &PaneTitle) -> Result<()> {
            self.inner.set_pane_title(pane_id, title)
        }

        fn set_option(
            &self,
            session: &TmuxName,
            key: &TmuxOptionName,
            value: &TmuxOptionValue,
        ) -> Result<()> {
            if self.fail_set_option {
                anyhow::bail!("set-option failed");
            }
            self.inner.set_option(session, key, value)
        }

        fn set_window_option(
            &self,
            session: &TmuxName,
            window: &TmuxName,
            key: &TmuxOptionName,
            value: &TmuxOptionValue,
        ) -> Result<()> {
            self.inner.set_window_option(session, window, key, value)
        }

        fn select_layout(
            &self,
            session: &TmuxName,
            window: &TmuxName,
            preset: &TmuxLayoutPreset,
        ) -> Result<()> {
            self.inner.select_layout(session, window, preset)
        }

        fn select_window(&self, session: &TmuxName, window: &TmuxName) -> Result<()> {
            self.inner.select_window(session, window)
        }

        fn attach_or_switch(&self, session: &TmuxName) -> Result<()> {
            self.inner.attach_or_switch(session)
        }

        fn kill_session(&self, name: &TmuxName) -> Result<()> {
            self.inner.kill_session(name)
        }

        fn rename_session(&self, old: &TmuxName, new: &TmuxName) -> Result<()> {
            self.inner.rename_session(old, new)
        }

        fn capture_pane(&self, pane_id: &PaneId) -> Result<String> {
            self.inner.capture_pane(pane_id)
        }

        fn run_command(&self, cmd: &ShellCommand) -> Result<()> {
            self.inner.run_command(cmd)
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
                        command: Some(pc("top")),
                        focus: false,
                        title: None,
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
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
    fn executor_reload_builds_replacement_before_moving_existing_session() {
        let b = RecordingBackend::new();
        b.session_exists.set(true);
        let session = simple_session("s", Some("vim"));
        let ex = Executor::new(&b);
        ex.reload(&session).unwrap();

        let calls = b.calls();
        let new_pos = calls
            .iter()
            .position(|c| c.starts_with("new-session:s__ntd_reload_new_"))
            .unwrap();
        let move_old_pos = calls
            .iter()
            .position(|c| c.starts_with("rename-session:s:s__ntd_reload_old_"))
            .unwrap();
        assert!(
            new_pos < move_old_pos,
            "replacement must be built before moving existing session"
        );
        assert!(calls
            .iter()
            .any(|c| c.starts_with("rename-session:s__ntd_reload_new_") && c.ends_with(":s")));
        assert!(calls
            .iter()
            .any(|c| c.starts_with("kill-session:s__ntd_reload_old_")));
        assert!(!calls.iter().any(|c| c == "kill-session:s"));
    }

    #[test]
    fn executor_reload_keeps_existing_session_when_replacement_creation_fails() {
        let b = FailingBackend {
            fail_set_option: true,
            ..Default::default()
        };
        b.inner.session_exists.set(true);
        let mut session = simple_session("s", None);
        session
            .options
            .insert(option_name("status"), option_value("off"));
        let ex = Executor::new(&b);
        let err = ex.reload(&session).unwrap_err();

        assert!(
            err.to_string().contains("failed to build replacement"),
            "{err}"
        );
        let calls = b.calls();
        assert!(!calls.iter().any(|c| c == "kill-session:s"));
        assert!(!calls.iter().any(|c| c.starts_with("rename-session:s:")));
        assert!(calls
            .iter()
            .any(|c| c.starts_with("kill-session:s__ntd_reload_new_")));
    }

    #[test]
    fn executor_reload_without_existing_session_creates_without_kill() {
        let b = RecordingBackend::new();
        let session = simple_session("s", Some("vim"));
        let ex = Executor::new(&b);
        ex.reload(&session).unwrap();

        let calls = b.calls();
        assert!(calls.iter().any(|c| c == "has-session:s"));
        assert!(!calls.iter().any(|c| c == "kill-session:s"));
        assert!(calls.iter().any(|c| c.starts_with("new-session:s:")));
    }

    #[test]
    fn executor_wait_for_success() {
        let b = RecordingBackend::new();
        *b.capture_output.borrow_mut() = "Server ready on port 3000".to_string();

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
                        timeout: WaitTimeoutSeconds::new(5).unwrap(),
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
                        timeout: WaitTimeoutSeconds::new(1).unwrap(),
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls.iter().any(|c| c == "set-option:s:status:off"),
            "set-option should be called for session options"
        );
    }

    #[test]
    fn executor_applies_session_options_in_key_order() {
        let b = RecordingBackend::new();
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
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        let base = call_pos(&calls, "set-option:s:base-index:1");
        let left = call_pos(&calls, "set-option:s:status-left:left");
        let right = call_pos(&calls, "set-option:s:status-right:right");
        assert!(
            base < left && left < right,
            "session options should follow key order: {calls:#?}"
        );
    }

    #[test]
    fn executor_window_options() {
        let b = RecordingBackend::new();
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
    fn executor_applies_window_options_in_key_order() {
        let b = RecordingBackend::new();
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
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        let automatic = call_pos(&calls, "set-window-option:s:w:automatic-rename:off");
        let synchronize = call_pos(&calls, "set-window-option:s:w:synchronize-panes:on");
        assert!(
            automatic < synchronize,
            "window options should follow key order: {calls:#?}"
        );
    }

    #[test]
    fn executor_select_layout() {
        let b = RecordingBackend::new();
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
                    title: Some(pt("my-title")),
                    wait_for: None,
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Pane {
                    command: Some(pc("cd {{cwd}}")),
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
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls.iter().any(|c| c == "send-keys:%0:cd $PWD"),
            "{{cwd}} should resolve to $PWD"
        );
    }

    #[test]
    fn executor_resolves_builtin_cwd_root_to_concrete_path() {
        let b = RecordingBackend::new();
        let mut session = simple_session("cwd-root", None);
        session.root = Some(root_template("{{cwd}}/project"));
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let cwd = std::env::current_dir()
            .unwrap()
            .into_os_string()
            .into_string()
            .expect("test cwd must be UTF-8");
        let expected = format!("new-session:cwd-root:{cwd}/project:main:%0");
        let calls = b.calls();
        assert!(
            calls.iter().any(|call| call == &expected),
            "root builtin should resolve before tmux argv boundary: {calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.contains("$PWD")),
            "tmux argv root must not receive shell syntax: {calls:?}"
        );
    }

    #[test]
    fn executor_multiple_windows_uses_new_window() {
        let b = RecordingBackend::new();
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![
                Window {
                    name: tmux_name("first"),
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
                },
                Window {
                    name: tmux_name("second"),
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
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::Split {
                    direction: Direction::Vertical,
                    ratio: ratio(0.5),
                    first: Box::new(LayoutNode::Pane {
                        command: Some(pc("top")),
                        focus: false,
                        title: None,
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
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
            pre_hook: Some(shell_command("nix build")),
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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
    fn executor_exports_session_and_window_env() {
        // Regression: previously the executor ignored session.env / window.env
        // entirely, so `run`/`reload` produced sessions WITHOUT the configured
        // environment — diverging from the `print`/compiler path.
        let b = RecordingBackend::new();
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
            windows: vec![Window {
                name: tmux_name("w"),
                root: None,
                env: vec![EnvVar {
                    key: env_key("WINDOW_VAR"),
                    value: env_value("wval"),
                }],
                options: BTreeMap::new(),
                select_layout: None,
                layout: LayoutNode::command("echo hi").unwrap(),
            }],
            env: vec![EnvVar {
                key: env_key("SESSION_VAR"),
                value: env_value("sval"),
            }],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        let new_session = calls
            .iter()
            .find(|c| c.starts_with("new-session:"))
            .cloned()
            .unwrap();
        assert!(
            new_session.contains(":env:SESSION_VAR=sval,WINDOW_VAR=wval"),
            "session and window env must be scoped to tmux pane creation: {new_session}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("set-env:")),
            "executor must not mutate ambient process env: {calls:?}"
        );
    }

    #[test]
    fn executor_rolls_back_initial_session_when_configuration_fails() {
        let b = FailingBackend {
            fail_set_option: true,
            ..Default::default()
        };
        let mut session = simple_session("s", None);
        session
            .options
            .insert(option_name("status"), option_value("off"));
        let ex = Executor::new(&b);

        let err = ex.run(&session).unwrap_err().to_string();
        assert!(
            err.contains("rolled back partially created tmux session"),
            "error should report rollback: {err}"
        );
        assert!(
            b.calls().iter().any(|c| c == "kill-session:s"),
            "partial session must be killed: {:?}",
            b.calls()
        );
    }

    #[test]
    fn executor_rolls_back_session_when_later_window_creation_fails() {
        let b = FailingBackend {
            fail_new_window: true,
            ..Default::default()
        };
        let mut session = simple_session("s", None);
        session.windows.push(Window {
            name: tmux_name("second"),
            root: None,
            env: vec![],
            options: BTreeMap::new(),
            select_layout: None,
            layout: LayoutNode::command("echo second").unwrap(),
        });
        let ex = Executor::new(&b);

        let err = ex.run(&session).unwrap_err().to_string();
        assert!(
            err.contains("rolled back partially created tmux session"),
            "error should report rollback: {err}"
        );
        assert!(
            b.calls().iter().any(|c| c == "kill-session:s"),
            "partial session must be killed: {:?}",
            b.calls()
        );
    }

    #[test]
    fn executor_window_root_overrides_session_root() {
        let b = RecordingBackend::new();
        let session = Session {
            name: tmux_name("s"),
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
            name: tmux_name("s"),
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
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        };
        let ex = Executor::new(&b);
        ex.run(&session).unwrap();

        let calls = b.calls();
        assert!(
            calls.iter().any(|c| c.contains('~')),
            "null root should fall back to ~"
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
                        timeout: WaitTimeoutSeconds::new(2).unwrap(),
                    }),
                },
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
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

    // ── Security: recursion depth limit ──────────────────────────────────────

    #[test]
    fn executor_depth_limit_rejects_oversized_tree() {
        use crate::test_fixtures::make_deeply_nested;
        let b = RecordingBackend::new();
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
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
        let ex = Executor::new(&b);
        let result = ex.run(&session);
        assert!(result.is_err(), "should fail for depth > MAX_LAYOUT_DEPTH");
        assert!(
            result.unwrap_err().to_string().contains("deeply nested"),
            "error should mention nesting"
        );
    }

    #[test]
    fn executor_depth_limit_accepts_depth_64() {
        use crate::test_fixtures::make_deeply_nested;
        let b = RecordingBackend::new();
        let session = Session {
            name: tmux_name("s"),
            root: Some(root_template("/tmp")),
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
        let ex = Executor::new(&b);
        assert!(ex.run(&session).is_ok(), "depth 64 should be accepted");
    }
}
