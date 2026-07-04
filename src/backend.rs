use crate::attach::{resolve_attach_action, AttachAction, AttachMode};
use crate::model::{
    EnvVar, PaneCommand, PaneId, PaneTitle, ResolvedTmuxArg, ShellCommand, TmuxLayoutPreset,
    TmuxName, TmuxOptionName, TmuxOptionValue, TmuxPanePercent, TmuxSplitFlag,
};
use anyhow::{Context, Result};
use std::cell::{Cell, RefCell};
use std::io::IsTerminal;
use std::process::{Command, Stdio};

// ─── Trait ────────────────────────────────────────────────────────────────────

pub trait TmuxBackend {
    fn has_session(&self, name: &TmuxName) -> bool;
    fn new_session(
        &self,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
        window_name: &TmuxName,
    ) -> Result<PaneId>;
    fn new_session_with_env(
        &self,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
        window_name: &TmuxName,
        _env: &[EnvVar],
    ) -> Result<PaneId> {
        self.new_session(name, root, window_name)
    }
    fn split_window(
        &self,
        pane_id: &PaneId,
        flag: TmuxSplitFlag,
        pct: TmuxPanePercent,
        root: &ResolvedTmuxArg,
    ) -> Result<PaneId>;
    fn split_window_with_env(
        &self,
        pane_id: &PaneId,
        flag: TmuxSplitFlag,
        pct: TmuxPanePercent,
        root: &ResolvedTmuxArg,
        _env: &[EnvVar],
    ) -> Result<PaneId> {
        self.split_window(pane_id, flag, pct, root)
    }
    fn new_window(
        &self,
        session: &TmuxName,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
    ) -> Result<PaneId>;
    fn new_window_with_env(
        &self,
        session: &TmuxName,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
        _env: &[EnvVar],
    ) -> Result<PaneId> {
        self.new_window(session, name, root)
    }
    fn send_keys(&self, pane_id: &PaneId, keys: &PaneCommand) -> Result<()>;
    fn select_pane(&self, pane_id: &PaneId) -> Result<()>;
    fn set_pane_title(&self, pane_id: &PaneId, title: &PaneTitle) -> Result<()>;
    fn set_option(
        &self,
        session: &TmuxName,
        key: &TmuxOptionName,
        value: &TmuxOptionValue,
    ) -> Result<()>;
    fn set_window_option(
        &self,
        session: &TmuxName,
        window: &TmuxName,
        key: &TmuxOptionName,
        value: &TmuxOptionValue,
    ) -> Result<()>;
    fn select_layout(
        &self,
        session: &TmuxName,
        window: &TmuxName,
        preset: &TmuxLayoutPreset,
    ) -> Result<()>;
    fn select_window(&self, session: &TmuxName, window: &TmuxName) -> Result<()>;
    fn attach_or_switch(&self, session: &TmuxName) -> Result<()>;
    fn kill_session(&self, name: &TmuxName) -> Result<()>;
    fn rename_session(&self, old: &TmuxName, new: &TmuxName) -> Result<()>;
    fn capture_pane(&self, pane_id: &PaneId) -> Result<String>;
    /// Run an arbitrary shell command (used for pre_hook execution).
    fn run_command(&self, cmd: &ShellCommand) -> Result<()>;
}

fn append_env_args(command: &mut Command, env: &[EnvVar]) {
    for ev in env {
        command.arg("-e").arg(format!("{}={}", ev.key, ev.value));
    }
}

fn parse_pane_id(stdout: &[u8], context: &str) -> Result<PaneId> {
    let pane_id = std::str::from_utf8(stdout)
        .with_context(|| format!("{context} returned non-UTF-8 pane id"))?
        .trim();
    PaneId::new(pane_id).with_context(|| format!("{context} returned invalid pane id"))
}

fn parse_capture_pane_output(stdout: &[u8], context: &str) -> Result<String> {
    Ok(std::str::from_utf8(stdout)
        .with_context(|| format!("{context} returned non-UTF-8 output"))?
        .to_owned())
}

// ─── RealTmux ─────────────────────────────────────────────────────────────────

/// Calls the real `tmux` binary.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealTmux {
    /// Whether — and when — to attach after building the session. See
    /// [`AttachMode`]. Defaults to [`AttachMode::Auto`].
    pub attach_mode: AttachMode,
}

impl RealTmux {
    /// Construct a backend with the given attach policy.
    pub fn new(attach_mode: AttachMode) -> Self {
        Self { attach_mode }
    }
}

impl TmuxBackend for RealTmux {
    fn has_session(&self, name: &TmuxName) -> bool {
        std::process::Command::new("tmux")
            .args(["has-session", "-t", name.as_str()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn new_session(
        &self,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
        window_name: &TmuxName,
    ) -> Result<PaneId> {
        self.new_session_with_env(name, root, window_name, &[])
    }

    fn new_session_with_env(
        &self,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
        window_name: &TmuxName,
        env: &[EnvVar],
    ) -> Result<PaneId> {
        let mut command = Command::new("tmux");
        command.args([
            "new-session",
            "-d",
            "-s",
            name.as_str(),
            "-c",
            root.as_str(),
            "-n",
            window_name.as_str(),
        ]);
        append_env_args(&mut command, env);
        let out = command.args(["-P", "-F", "#{pane_id}"]).output()?;
        if !out.status.success() {
            anyhow::bail!(
                "tmux new-session failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        parse_pane_id(&out.stdout, "tmux new-session")
    }

    fn split_window(
        &self,
        pane_id: &PaneId,
        flag: TmuxSplitFlag,
        pct: TmuxPanePercent,
        root: &ResolvedTmuxArg,
    ) -> Result<PaneId> {
        self.split_window_with_env(pane_id, flag, pct, root, &[])
    }

    fn split_window_with_env(
        &self,
        pane_id: &PaneId,
        flag: TmuxSplitFlag,
        pct: TmuxPanePercent,
        root: &ResolvedTmuxArg,
        env: &[EnvVar],
    ) -> Result<PaneId> {
        let pct_str = pct.as_tmux_size();
        let mut command = Command::new("tmux");
        command.args([
            "split-window",
            "-t",
            pane_id.as_str(),
            flag.as_str(),
            "-l",
            &pct_str,
            "-c",
            root.as_str(),
        ]);
        append_env_args(&mut command, env);
        let out = command.args(["-P", "-F", "#{pane_id}"]).output()?;
        if !out.status.success() {
            anyhow::bail!(
                "tmux split-window failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        parse_pane_id(&out.stdout, "tmux split-window")
    }

    fn new_window(
        &self,
        session: &TmuxName,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
    ) -> Result<PaneId> {
        self.new_window_with_env(session, name, root, &[])
    }

    fn new_window_with_env(
        &self,
        session: &TmuxName,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
        env: &[EnvVar],
    ) -> Result<PaneId> {
        let mut command = Command::new("tmux");
        command.args([
            "new-window",
            "-t",
            session.as_str(),
            "-n",
            name.as_str(),
            "-c",
            root.as_str(),
        ]);
        append_env_args(&mut command, env);
        let out = command.args(["-P", "-F", "#{pane_id}"]).output()?;
        if !out.status.success() {
            anyhow::bail!(
                "tmux new-window failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        parse_pane_id(&out.stdout, "tmux new-window")
    }

    fn send_keys(&self, pane_id: &PaneId, keys: &PaneCommand) -> Result<()> {
        let literal_status = Command::new("tmux")
            .args([
                "send-keys",
                "-t",
                pane_id.as_str(),
                "-l",
                "--",
                keys.as_str(),
            ])
            .status()?;
        anyhow::ensure!(
            literal_status.success(),
            "tmux send-keys literal failed: exit code {:?}",
            literal_status.code()
        );
        let enter_status = Command::new("tmux")
            .args(["send-keys", "-t", pane_id.as_str(), "Enter"])
            .status()?;
        anyhow::ensure!(
            enter_status.success(),
            "tmux send-keys Enter failed: exit code {:?}",
            enter_status.code()
        );
        Ok(())
    }

    fn select_pane(&self, pane_id: &PaneId) -> Result<()> {
        let status = std::process::Command::new("tmux")
            .args(["select-pane", "-t", pane_id.as_str()])
            .status()?;
        anyhow::ensure!(
            status.success(),
            "tmux select-pane failed: exit code {:?}",
            status.code()
        );
        Ok(())
    }

    fn set_pane_title(&self, pane_id: &PaneId, title: &PaneTitle) -> Result<()> {
        let status = std::process::Command::new("tmux")
            .args(["select-pane", "-t", pane_id.as_str(), "-T", title.as_str()])
            .status()?;
        anyhow::ensure!(
            status.success(),
            "tmux set-pane-title failed: exit code {:?}",
            status.code()
        );
        Ok(())
    }

    fn set_option(
        &self,
        session: &TmuxName,
        key: &TmuxOptionName,
        value: &TmuxOptionValue,
    ) -> Result<()> {
        // "--" prevents a key starting with '-' from being parsed as a tmux flag
        let status = std::process::Command::new("tmux")
            .args([
                "set-option",
                "-t",
                session.as_str(),
                "--",
                key.as_str(),
                value.as_str(),
            ])
            .status()?;
        anyhow::ensure!(
            status.success(),
            "tmux set-option failed: exit code {:?}",
            status.code()
        );
        Ok(())
    }

    fn set_window_option(
        &self,
        session: &TmuxName,
        window: &TmuxName,
        key: &TmuxOptionName,
        value: &TmuxOptionValue,
    ) -> Result<()> {
        let target = format!("{}:{}", session.as_str(), window.as_str());
        // "--" prevents a key starting with '-' from being parsed as a tmux flag
        let status = std::process::Command::new("tmux")
            .args([
                "set-window-option",
                "-t",
                &target,
                "--",
                key.as_str(),
                value.as_str(),
            ])
            .status()?;
        anyhow::ensure!(
            status.success(),
            "tmux set-window-option failed: exit code {:?}",
            status.code()
        );
        Ok(())
    }

    fn select_layout(
        &self,
        session: &TmuxName,
        window: &TmuxName,
        preset: &TmuxLayoutPreset,
    ) -> Result<()> {
        let target = format!("{}:{}", session.as_str(), window.as_str());
        let status = std::process::Command::new("tmux")
            .args(["select-layout", "-t", &target, "--", preset.as_str()])
            .status()?;
        anyhow::ensure!(
            status.success(),
            "tmux select-layout failed: exit code {:?}",
            status.code()
        );
        Ok(())
    }

    fn select_window(&self, session: &TmuxName, window: &TmuxName) -> Result<()> {
        let target = format!("{}:{}", session.as_str(), window.as_str());
        let status = std::process::Command::new("tmux")
            .args(["select-window", "-t", &target])
            .status()?;
        anyhow::ensure!(
            status.success(),
            "tmux select-window failed: exit code {:?}",
            status.code()
        );
        Ok(())
    }

    fn attach_or_switch(&self, session: &TmuxName) -> Result<()> {
        let action = resolve_attach_action(
            self.attach_mode,
            std::env::var_os("TMUX").is_some(),
            std::io::stdin().is_terminal(),
        );
        let status = match action {
            AttachAction::Switch => Command::new("tmux")
                .args(["switch-client", "-t", session.as_str()])
                .status()?,
            AttachAction::Attach => Command::new("tmux")
                .args(["attach-session", "-t", session.as_str()])
                .status()?,
            AttachAction::Skip => {
                // The session is built and detached; there is just no terminal
                // to hand over (headless caller / --no-attach). Report where it
                // went instead of failing the command.
                eprintln!(
                    "nix-tmux-define: session {:?} is ready (not attaching). \
                     Attach later with: tmux attach -t {}",
                    session.as_str(),
                    session.as_str()
                );
                return Ok(());
            }
        };
        anyhow::ensure!(
            status.success(),
            "tmux attach/switch failed: exit code {:?}",
            status.code()
        );
        Ok(())
    }

    fn kill_session(&self, name: &TmuxName) -> Result<()> {
        let status = std::process::Command::new("tmux")
            .args(["kill-session", "-t", name.as_str()])
            .status()?;
        anyhow::ensure!(
            status.success(),
            "tmux kill-session failed: exit code {:?}",
            status.code()
        );
        Ok(())
    }

    fn rename_session(&self, old: &TmuxName, new: &TmuxName) -> Result<()> {
        let out = std::process::Command::new("tmux")
            .args(["rename-session", "-t", old.as_str(), new.as_str()])
            .output()?;
        if !out.status.success() {
            anyhow::bail!(
                "tmux rename-session failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    fn capture_pane(&self, pane_id: &PaneId) -> Result<String> {
        let out = std::process::Command::new("tmux")
            .args(["capture-pane", "-t", pane_id.as_str(), "-p"])
            .output()?;
        if !out.status.success() {
            anyhow::bail!(
                "tmux capture-pane failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        parse_capture_pane_output(&out.stdout, "tmux capture-pane")
    }

    fn run_command(&self, cmd: &ShellCommand) -> Result<()> {
        let status = Command::new("bash").args(["-c", cmd.as_str()]).status()?;
        anyhow::ensure!(
            status.success(),
            "pre_hook command failed: exit code {:?}",
            status.code()
        );
        Ok(())
    }
}

// ─── RecordingBackend ─────────────────────────────────────────────────────────

/// A test double that records all backend calls without invoking real tmux.
#[derive(Debug, Default)]
pub struct RecordingBackend {
    calls: RefCell<Vec<String>>,
    pane_counter: Cell<usize>,
    pub session_exists: Cell<bool>,
    pub capture_output: RefCell<String>,
}

impl RecordingBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn record(&self, s: impl Into<String>) {
        self.calls.borrow_mut().push(s.into());
    }

    fn next_pane(&self) -> String {
        let n = self.pane_counter.get();
        self.pane_counter.set(n + 1);
        format!("%{}", n)
    }

    fn env_suffix(env: &[EnvVar]) -> String {
        if env.is_empty() {
            String::new()
        } else {
            let pairs = env
                .iter()
                .map(|ev| format!("{}={}", ev.key, ev.value))
                .collect::<Vec<_>>()
                .join(",");
            format!(":env:{}", pairs)
        }
    }
}

impl TmuxBackend for RecordingBackend {
    fn has_session(&self, name: &TmuxName) -> bool {
        self.record(format!("has-session:{}", name.as_str()));
        self.session_exists.get()
    }

    fn new_session(
        &self,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
        window_name: &TmuxName,
    ) -> Result<PaneId> {
        self.new_session_with_env(name, root, window_name, &[])
    }

    fn new_session_with_env(
        &self,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
        window_name: &TmuxName,
        env: &[EnvVar],
    ) -> Result<PaneId> {
        let id = PaneId::new(self.next_pane())?;
        self.record(format!(
            "new-session:{}:{}:{}:{}{}",
            name.as_str(),
            root.as_str(),
            window_name.as_str(),
            id.as_str(),
            Self::env_suffix(env)
        ));
        Ok(id)
    }

    fn split_window(
        &self,
        pane_id: &PaneId,
        flag: TmuxSplitFlag,
        pct: TmuxPanePercent,
        root: &ResolvedTmuxArg,
    ) -> Result<PaneId> {
        self.split_window_with_env(pane_id, flag, pct, root, &[])
    }

    fn split_window_with_env(
        &self,
        pane_id: &PaneId,
        flag: TmuxSplitFlag,
        pct: TmuxPanePercent,
        root: &ResolvedTmuxArg,
        env: &[EnvVar],
    ) -> Result<PaneId> {
        let id = PaneId::new(self.next_pane())?;
        self.record(format!(
            "split-window:{}:{}:{}:{}:{}{}",
            pane_id.as_str(),
            flag.as_str(),
            pct.as_u32(),
            root.as_str(),
            id.as_str(),
            Self::env_suffix(env)
        ));
        Ok(id)
    }

    fn new_window(
        &self,
        session: &TmuxName,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
    ) -> Result<PaneId> {
        self.new_window_with_env(session, name, root, &[])
    }

    fn new_window_with_env(
        &self,
        session: &TmuxName,
        name: &TmuxName,
        root: &ResolvedTmuxArg,
        env: &[EnvVar],
    ) -> Result<PaneId> {
        let id = PaneId::new(self.next_pane())?;
        self.record(format!(
            "new-window:{}:{}:{}:{}{}",
            session.as_str(),
            name.as_str(),
            root.as_str(),
            id.as_str(),
            Self::env_suffix(env)
        ));
        Ok(id)
    }

    fn send_keys(&self, pane_id: &PaneId, keys: &PaneCommand) -> Result<()> {
        self.record(format!("send-keys:{}:{}", pane_id.as_str(), keys.as_str()));
        Ok(())
    }

    fn select_pane(&self, pane_id: &PaneId) -> Result<()> {
        self.record(format!("select-pane:{}", pane_id.as_str()));
        Ok(())
    }

    fn set_pane_title(&self, pane_id: &PaneId, title: &PaneTitle) -> Result<()> {
        self.record(format!(
            "set-pane-title:{}:{}",
            pane_id.as_str(),
            title.as_str()
        ));
        Ok(())
    }

    fn set_option(
        &self,
        session: &TmuxName,
        key: &TmuxOptionName,
        value: &TmuxOptionValue,
    ) -> Result<()> {
        self.record(format!(
            "set-option:{}:{}:{}",
            session.as_str(),
            key.as_str(),
            value.as_str()
        ));
        Ok(())
    }

    fn set_window_option(
        &self,
        session: &TmuxName,
        window: &TmuxName,
        key: &TmuxOptionName,
        value: &TmuxOptionValue,
    ) -> Result<()> {
        self.record(format!(
            "set-window-option:{}:{}:{}:{}",
            session.as_str(),
            window.as_str(),
            key.as_str(),
            value.as_str()
        ));
        Ok(())
    }

    fn select_layout(
        &self,
        session: &TmuxName,
        window: &TmuxName,
        preset: &TmuxLayoutPreset,
    ) -> Result<()> {
        self.record(format!(
            "select-layout:{}:{}:{}",
            session.as_str(),
            window.as_str(),
            preset.as_str()
        ));
        Ok(())
    }

    fn select_window(&self, session: &TmuxName, window: &TmuxName) -> Result<()> {
        self.record(format!(
            "select-window:{}:{}",
            session.as_str(),
            window.as_str()
        ));
        Ok(())
    }

    fn attach_or_switch(&self, session: &TmuxName) -> Result<()> {
        self.record(format!("attach-or-switch:{}", session.as_str()));
        Ok(())
    }

    fn kill_session(&self, name: &TmuxName) -> Result<()> {
        self.record(format!("kill-session:{}", name.as_str()));
        self.session_exists.set(false);
        Ok(())
    }

    fn rename_session(&self, old: &TmuxName, new: &TmuxName) -> Result<()> {
        self.record(format!("rename-session:{}:{}", old.as_str(), new.as_str()));
        self.session_exists.set(true);
        Ok(())
    }

    fn capture_pane(&self, pane_id: &PaneId) -> Result<String> {
        self.record(format!("capture-pane:{}", pane_id.as_str()));
        Ok(self.capture_output.borrow().clone())
    }

    fn run_command(&self, cmd: &ShellCommand) -> Result<()> {
        self.record(format!("run-command:{}", cmd.as_str()));
        Ok(())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::model::{EnvVarName, EnvVarValue};

    use super::*;

    fn root() -> ResolvedTmuxArg {
        ResolvedTmuxArg::new("/tmp").unwrap()
    }

    fn pane(id: &str) -> PaneId {
        PaneId::new(id).unwrap()
    }

    fn tmux_name(name: &str) -> TmuxName {
        TmuxName::new(name).unwrap()
    }

    fn pane_command(value: &str) -> PaneCommand {
        PaneCommand::new(value).unwrap()
    }

    fn pane_title(value: &str) -> PaneTitle {
        PaneTitle::new(value).unwrap()
    }

    fn tmux_option_name(value: &str) -> TmuxOptionName {
        TmuxOptionName::new(value).unwrap()
    }

    fn tmux_option_value(value: &str) -> TmuxOptionValue {
        TmuxOptionValue::new(value).unwrap()
    }

    fn tmux_layout_preset(value: &str) -> TmuxLayoutPreset {
        TmuxLayoutPreset::new(value).unwrap()
    }

    fn env_key(value: &str) -> EnvVarName {
        EnvVarName::new(value).unwrap()
    }

    fn env_value(value: &str) -> EnvVarValue {
        EnvVarValue::new(value).unwrap()
    }

    fn shell_command(value: &str) -> ShellCommand {
        ShellCommand::new(value).unwrap()
    }

    #[test]
    fn parse_pane_id_accepts_valid_utf8_output_with_newline() {
        let id = parse_pane_id(b"%42\n", "tmux new-session").unwrap();
        assert_eq!(id.as_str(), "%42");
    }

    #[test]
    fn parse_pane_id_rejects_non_utf8_output() {
        let err = parse_pane_id(b"%4\xff\n", "tmux new-session").unwrap_err();
        assert!(
            err.to_string().contains("non-UTF-8 pane id"),
            "error should mention UTF-8: {err:#}"
        );
    }

    #[test]
    fn parse_pane_id_rejects_non_tmux_id() {
        let err = parse_pane_id(b"not-pane\n", "tmux new-session").unwrap_err();
        assert!(
            err.to_string().contains("returned invalid pane id"),
            "error should mention invalid pane id: {err:#}"
        );
    }

    #[test]
    fn parse_capture_pane_output_accepts_valid_utf8() {
        let output = parse_capture_pane_output(b"ready\n", "tmux capture-pane").unwrap();
        assert_eq!(output, "ready\n");
    }

    #[test]
    fn parse_capture_pane_output_rejects_non_utf8() {
        let err = parse_capture_pane_output(b"ready\xff\n", "tmux capture-pane").unwrap_err();
        assert!(
            err.to_string().contains("non-UTF-8 output"),
            "error should mention UTF-8: {err:#}"
        );
    }

    #[test]
    fn recording_backend_has_session_default_false() {
        let b = RecordingBackend::new();
        assert!(!b.has_session(&tmux_name("mysession")));
        assert!(b.calls().iter().any(|c| c == "has-session:mysession"));
    }

    #[test]
    fn recording_backend_session_exists_true() {
        let b = RecordingBackend::new();
        b.session_exists.set(true);
        assert!(b.has_session(&tmux_name("mysession")));
    }

    #[test]
    fn recording_backend_new_session() {
        let b = RecordingBackend::new();
        let id = b
            .new_session(&tmux_name("s"), &root(), &tmux_name("main"))
            .unwrap();
        assert_eq!(id.as_str(), "%0");
        assert!(b
            .calls()
            .iter()
            .any(|c| c.starts_with("new-session:s:/tmp:main:")));
    }

    #[test]
    fn recording_backend_split_window() {
        let b = RecordingBackend::new();
        let id = b
            .split_window(
                &pane("%0"),
                TmuxSplitFlag::Horizontal,
                TmuxPanePercent::new(40).unwrap(),
                &root(),
            )
            .unwrap();
        assert_eq!(id.as_str(), "%0");
        assert!(b
            .calls()
            .iter()
            .any(|c| c.starts_with("split-window:%0:-h:40:/tmp:")));
    }

    #[test]
    fn recording_backend_new_window() {
        let b = RecordingBackend::new();
        let id = b
            .new_window(&tmux_name("s"), &tmux_name("logs"), &root())
            .unwrap();
        assert_eq!(id.as_str(), "%0");
        assert!(b
            .calls()
            .iter()
            .any(|c| c.starts_with("new-window:s:logs:/tmp:")));
    }

    #[test]
    fn recording_backend_send_keys() {
        let b = RecordingBackend::new();
        b.send_keys(&pane("%0"), &pane_command("vim .")).unwrap();
        assert!(b.calls().iter().any(|c| c == "send-keys:%0:vim ."));
    }

    #[test]
    fn recording_backend_select_pane() {
        let b = RecordingBackend::new();
        b.select_pane(&pane("%0")).unwrap();
        assert!(b.calls().iter().any(|c| c == "select-pane:%0"));
    }

    #[test]
    fn recording_backend_set_pane_title() {
        let b = RecordingBackend::new();
        b.set_pane_title(&pane("%0"), &pane_title("editor"))
            .unwrap();
        assert!(b.calls().iter().any(|c| c == "set-pane-title:%0:editor"));
    }

    #[test]
    fn recording_backend_set_option() {
        let b = RecordingBackend::new();
        b.set_option(
            &tmux_name("s"),
            &tmux_option_name("status"),
            &tmux_option_value("off"),
        )
        .unwrap();
        assert!(b.calls().iter().any(|c| c == "set-option:s:status:off"));
    }

    #[test]
    fn recording_backend_set_window_option() {
        let b = RecordingBackend::new();
        b.set_window_option(
            &tmux_name("s"),
            &tmux_name("w"),
            &tmux_option_name("sync"),
            &tmux_option_value("on"),
        )
        .unwrap();
        assert!(b
            .calls()
            .iter()
            .any(|c| c == "set-window-option:s:w:sync:on"));
    }

    #[test]
    fn recording_backend_select_layout() {
        let b = RecordingBackend::new();
        b.select_layout(
            &tmux_name("s"),
            &tmux_name("w"),
            &tmux_layout_preset("tiled"),
        )
        .unwrap();
        assert!(b.calls().iter().any(|c| c == "select-layout:s:w:tiled"));
    }

    #[test]
    fn recording_backend_select_window() {
        let b = RecordingBackend::new();
        b.select_window(&tmux_name("s"), &tmux_name("main"))
            .unwrap();
        assert!(b.calls().iter().any(|c| c == "select-window:s:main"));
    }

    #[test]
    fn recording_backend_attach_or_switch() {
        let b = RecordingBackend::new();
        b.attach_or_switch(&tmux_name("s")).unwrap();
        assert!(b.calls().iter().any(|c| c == "attach-or-switch:s"));
    }

    #[test]
    fn recording_backend_kill_session_clears_flag() {
        let b = RecordingBackend::new();
        b.session_exists.set(true);
        b.kill_session(&tmux_name("s")).unwrap();
        assert!(!b.session_exists.get());
        assert!(b.calls().iter().any(|c| c == "kill-session:s"));
    }

    #[test]
    fn recording_backend_rename_session() {
        let b = RecordingBackend::new();
        b.rename_session(&tmux_name("old"), &tmux_name("new"))
            .unwrap();
        assert!(b.calls().iter().any(|c| c == "rename-session:old:new"));
    }

    #[test]
    fn recording_backend_capture_pane() {
        let b = RecordingBackend::new();
        *b.capture_output.borrow_mut() = "hello world".into();
        let out = b.capture_pane(&pane("%0")).unwrap();
        assert_eq!(out, "hello world");
        assert!(b.calls().iter().any(|c| c == "capture-pane:%0"));
    }

    #[test]
    fn recording_backend_run_command() {
        let b = RecordingBackend::new();
        b.run_command(&shell_command("echo hi")).unwrap();
        assert!(b.calls().iter().any(|c| c == "run-command:echo hi"));
    }

    #[test]
    fn recording_backend_new_session_with_env() {
        let b = RecordingBackend::new();
        let env = [EnvVar {
            key: env_key("EDITOR"),
            value: env_value("nvim"),
        }];
        b.new_session_with_env(&tmux_name("s"), &root(), &tmux_name("main"), &env)
            .unwrap();
        assert!(b
            .calls()
            .iter()
            .any(|c| c == "new-session:s:/tmp:main:%0:env:EDITOR=nvim"));
    }
}
