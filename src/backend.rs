use anyhow::Result;
use std::cell::{Cell, RefCell};

// ─── Trait ────────────────────────────────────────────────────────────────────

pub trait TmuxBackend {
    fn has_session(&self, name: &str) -> bool;
    fn new_session(&self, name: &str, root: &str, window_name: &str) -> Result<String>;
    fn split_window(&self, pane_id: &str, flag: &str, pct: u32, root: &str) -> Result<String>;
    fn new_window(&self, session: &str, name: &str, root: &str) -> Result<String>;
    fn send_keys(&self, pane_id: &str, keys: &str) -> Result<()>;
    fn select_pane(&self, pane_id: &str) -> Result<()>;
    fn set_pane_title(&self, pane_id: &str, title: &str) -> Result<()>;
    fn set_option(&self, session: &str, key: &str, value: &str) -> Result<()>;
    fn set_window_option(&self, session: &str, window: &str, key: &str, value: &str) -> Result<()>;
    fn select_layout(&self, session: &str, window: &str, preset: &str) -> Result<()>;
    fn select_window(&self, session: &str, index: usize) -> Result<()>;
    fn attach_or_switch(&self, session: &str) -> Result<()>;
    fn kill_session(&self, name: &str) -> Result<()>;
    fn capture_pane(&self, pane_id: &str) -> Result<String>;
    /// Run an arbitrary shell command (used for pre_hook execution).
    fn run_command(&self, cmd: &str) -> Result<()>;
}

// ─── RealTmux ─────────────────────────────────────────────────────────────────

/// Calls the real `tmux` binary.
pub struct RealTmux;

impl TmuxBackend for RealTmux {
    fn has_session(&self, name: &str) -> bool {
        std::process::Command::new("tmux")
            .args(["has-session", "-t", name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn new_session(&self, name: &str, root: &str, window_name: &str) -> Result<String> {
        let out = std::process::Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                name,
                "-c",
                root,
                "-n",
                window_name,
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn split_window(&self, pane_id: &str, flag: &str, pct: u32, root: &str) -> Result<String> {
        let pct_str = format!("{}%", pct);
        let out = std::process::Command::new("tmux")
            .args([
                "split-window",
                "-t",
                pane_id,
                flag,
                "-l",
                &pct_str,
                "-c",
                root,
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn new_window(&self, session: &str, name: &str, root: &str) -> Result<String> {
        let out = std::process::Command::new("tmux")
            .args([
                "new-window", "-t", session, "-n", name, "-c", root, "-P", "-F", "#{pane_id}",
            ])
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn send_keys(&self, pane_id: &str, keys: &str) -> Result<()> {
        std::process::Command::new("tmux")
            .args(["send-keys", "-t", pane_id, keys, "Enter"])
            .status()?;
        Ok(())
    }

    fn select_pane(&self, pane_id: &str) -> Result<()> {
        std::process::Command::new("tmux")
            .args(["select-pane", "-t", pane_id])
            .status()?;
        Ok(())
    }

    fn set_pane_title(&self, pane_id: &str, title: &str) -> Result<()> {
        std::process::Command::new("tmux")
            .args(["select-pane", "-t", pane_id, "-T", title])
            .status()?;
        Ok(())
    }

    fn set_option(&self, session: &str, key: &str, value: &str) -> Result<()> {
        std::process::Command::new("tmux")
            .args(["set-option", "-t", session, key, value])
            .status()?;
        Ok(())
    }

    fn set_window_option(&self, session: &str, window: &str, key: &str, value: &str) -> Result<()> {
        let target = format!("{}:{}", session, window);
        std::process::Command::new("tmux")
            .args(["set-window-option", "-t", &target, key, value])
            .status()?;
        Ok(())
    }

    fn select_layout(&self, session: &str, window: &str, preset: &str) -> Result<()> {
        let target = format!("{}:{}", session, window);
        std::process::Command::new("tmux")
            .args(["select-layout", "-t", &target, preset])
            .status()?;
        Ok(())
    }

    fn select_window(&self, session: &str, index: usize) -> Result<()> {
        let target = format!("{}:{}", session, index);
        std::process::Command::new("tmux")
            .args(["select-window", "-t", &target])
            .status()?;
        Ok(())
    }

    fn attach_or_switch(&self, session: &str) -> Result<()> {
        if std::env::var("TMUX").is_ok() {
            std::process::Command::new("tmux")
                .args(["switch-client", "-t", session])
                .status()?;
        } else {
            std::process::Command::new("tmux")
                .args(["attach-session", "-t", session])
                .status()?;
        }
        Ok(())
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        std::process::Command::new("tmux")
            .args(["kill-session", "-t", name])
            .status()?;
        Ok(())
    }

    fn capture_pane(&self, pane_id: &str) -> Result<String> {
        let out = std::process::Command::new("tmux")
            .args(["capture-pane", "-t", pane_id, "-p"])
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    fn run_command(&self, cmd: &str) -> Result<()> {
        std::process::Command::new("bash").args(["-c", cmd]).status()?;
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
}

impl TmuxBackend for RecordingBackend {
    fn has_session(&self, name: &str) -> bool {
        self.record(format!("has-session:{}", name));
        self.session_exists.get()
    }

    fn new_session(&self, name: &str, root: &str, window_name: &str) -> Result<String> {
        let id = self.next_pane();
        self.record(format!("new-session:{}:{}:{}:{}", name, root, window_name, id));
        Ok(id)
    }

    fn split_window(&self, pane_id: &str, flag: &str, pct: u32, root: &str) -> Result<String> {
        let id = self.next_pane();
        self.record(format!("split-window:{}:{}:{}:{}:{}", pane_id, flag, pct, root, id));
        Ok(id)
    }

    fn new_window(&self, session: &str, name: &str, root: &str) -> Result<String> {
        let id = self.next_pane();
        self.record(format!("new-window:{}:{}:{}:{}", session, name, root, id));
        Ok(id)
    }

    fn send_keys(&self, pane_id: &str, keys: &str) -> Result<()> {
        self.record(format!("send-keys:{}:{}", pane_id, keys));
        Ok(())
    }

    fn select_pane(&self, pane_id: &str) -> Result<()> {
        self.record(format!("select-pane:{}", pane_id));
        Ok(())
    }

    fn set_pane_title(&self, pane_id: &str, title: &str) -> Result<()> {
        self.record(format!("set-pane-title:{}:{}", pane_id, title));
        Ok(())
    }

    fn set_option(&self, session: &str, key: &str, value: &str) -> Result<()> {
        self.record(format!("set-option:{}:{}:{}", session, key, value));
        Ok(())
    }

    fn set_window_option(&self, session: &str, window: &str, key: &str, value: &str) -> Result<()> {
        self.record(format!("set-window-option:{}:{}:{}:{}", session, window, key, value));
        Ok(())
    }

    fn select_layout(&self, session: &str, window: &str, preset: &str) -> Result<()> {
        self.record(format!("select-layout:{}:{}:{}", session, window, preset));
        Ok(())
    }

    fn select_window(&self, session: &str, index: usize) -> Result<()> {
        self.record(format!("select-window:{}:{}", session, index));
        Ok(())
    }

    fn attach_or_switch(&self, session: &str) -> Result<()> {
        self.record(format!("attach-or-switch:{}", session));
        Ok(())
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        self.record(format!("kill-session:{}", name));
        self.session_exists.set(false);
        Ok(())
    }

    fn capture_pane(&self, pane_id: &str) -> Result<String> {
        self.record(format!("capture-pane:{}", pane_id));
        Ok(self.capture_output.borrow().clone())
    }

    fn run_command(&self, cmd: &str) -> Result<()> {
        self.record(format!("run-command:{}", cmd));
        Ok(())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_backend_has_session_default_false() {
        let b = RecordingBackend::new();
        assert!(!b.has_session("mysession"));
        assert!(b.calls().iter().any(|c| c == "has-session:mysession"));
    }

    #[test]
    fn recording_backend_session_exists_true() {
        let b = RecordingBackend::new();
        b.session_exists.set(true);
        assert!(b.has_session("mysession"));
    }

    #[test]
    fn recording_backend_new_session() {
        let b = RecordingBackend::new();
        let id = b.new_session("s", "/tmp", "main").unwrap();
        assert_eq!(id, "%0");
        assert!(b.calls().iter().any(|c| c.starts_with("new-session:s:/tmp:main:")));
    }

    #[test]
    fn recording_backend_split_window() {
        let b = RecordingBackend::new();
        let id = b.split_window("%0", "-h", 40, "/tmp").unwrap();
        assert_eq!(id, "%0");
        assert!(b.calls().iter().any(|c| c.starts_with("split-window:%0:-h:40:/tmp:")));
    }

    #[test]
    fn recording_backend_new_window() {
        let b = RecordingBackend::new();
        let id = b.new_window("s", "logs", "/tmp").unwrap();
        assert_eq!(id, "%0");
        assert!(b.calls().iter().any(|c| c.starts_with("new-window:s:logs:/tmp:")));
    }

    #[test]
    fn recording_backend_send_keys() {
        let b = RecordingBackend::new();
        b.send_keys("%0", "vim .").unwrap();
        assert!(b.calls().iter().any(|c| c == "send-keys:%0:vim ."));
    }

    #[test]
    fn recording_backend_select_pane() {
        let b = RecordingBackend::new();
        b.select_pane("%0").unwrap();
        assert!(b.calls().iter().any(|c| c == "select-pane:%0"));
    }

    #[test]
    fn recording_backend_set_pane_title() {
        let b = RecordingBackend::new();
        b.set_pane_title("%0", "editor").unwrap();
        assert!(b.calls().iter().any(|c| c == "set-pane-title:%0:editor"));
    }

    #[test]
    fn recording_backend_set_option() {
        let b = RecordingBackend::new();
        b.set_option("s", "status", "off").unwrap();
        assert!(b.calls().iter().any(|c| c == "set-option:s:status:off"));
    }

    #[test]
    fn recording_backend_set_window_option() {
        let b = RecordingBackend::new();
        b.set_window_option("s", "w", "sync", "on").unwrap();
        assert!(b.calls().iter().any(|c| c == "set-window-option:s:w:sync:on"));
    }

    #[test]
    fn recording_backend_select_layout() {
        let b = RecordingBackend::new();
        b.select_layout("s", "w", "tiled").unwrap();
        assert!(b.calls().iter().any(|c| c == "select-layout:s:w:tiled"));
    }

    #[test]
    fn recording_backend_select_window() {
        let b = RecordingBackend::new();
        b.select_window("s", 2).unwrap();
        assert!(b.calls().iter().any(|c| c == "select-window:s:2"));
    }

    #[test]
    fn recording_backend_attach_or_switch() {
        let b = RecordingBackend::new();
        b.attach_or_switch("s").unwrap();
        assert!(b.calls().iter().any(|c| c == "attach-or-switch:s"));
    }

    #[test]
    fn recording_backend_kill_session_clears_flag() {
        let b = RecordingBackend::new();
        b.session_exists.set(true);
        b.kill_session("s").unwrap();
        assert!(!b.session_exists.get());
        assert!(b.calls().iter().any(|c| c == "kill-session:s"));
    }

    #[test]
    fn recording_backend_capture_pane() {
        let b = RecordingBackend::new();
        *b.capture_output.borrow_mut() = "hello world".into();
        let out = b.capture_pane("%0").unwrap();
        assert_eq!(out, "hello world");
        assert!(b.calls().iter().any(|c| c == "capture-pane:%0"));
    }

    #[test]
    fn recording_backend_run_command() {
        let b = RecordingBackend::new();
        b.run_command("echo hi").unwrap();
        assert!(b.calls().iter().any(|c| c == "run-command:echo hi"));
    }
}
