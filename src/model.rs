use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Model ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct Session {
    pub name: String,
    /// Default working directory for all panes; falls back to `$HOME`
    pub root: Option<String>,
    pub windows: Vec<Window>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Shell command executed before the session is created (e.g. `nix build`)
    pub pre_hook: Option<String>,
    /// tmux set-option key/value pairs for the session
    #[serde(default)]
    pub options: HashMap<String, String>,
    /// Template variables substituted via {{key}} in commands/roots
    #[serde(default)]
    pub vars: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct Window {
    pub name: String,
    pub layout: LayoutNode,
    /// Working directory for this window; overrides the session root when set
    pub root: Option<String>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// tmux set-window-option key/value pairs for this window
    #[serde(default)]
    pub options: HashMap<String, String>,
    /// Layout preset (e.g. "tiled", "even-horizontal", "even-vertical")
    pub select_layout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

/// A node in the recursive pane-layout tree.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
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
        /// Wait for a pattern in pane output before continuing
        #[serde(default)]
        wait_for: Option<WaitFor>,
    },
    Split {
        direction: Direction,
        /// Fraction of space [0.0, 1.0] allocated to the *first* child
        ratio: f64,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct WaitFor {
    pub pattern: String,
    #[serde(default = "default_timeout")]
    pub timeout: u32,
}

fn default_timeout() -> u32 {
    30
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
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

// ─── Template Variable Resolution ────────────────────────────────────────────

/// Substitutes `{{key}}` from the `vars` map, then built-in dynamic vars:
/// - `{{cwd}}` → `$PWD`
/// - `{{date}}` → `$(date +%Y-%m-%d)`
/// - `{{git_branch}}` → `$(git rev-parse --abbrev-ref HEAD)`
pub fn resolve_vars(s: &str, vars: &HashMap<String, String>) -> String {
    let mut result = s.to_string();
    // User-defined vars first
    for (k, v) in vars {
        result = result.replace(&format!("{{{{{}}}}}", k), v);
    }
    // Built-in dynamic vars
    result = result.replace("{{cwd}}", "$PWD");
    result = result.replace("{{date}}", "$(date +%Y-%m-%d)");
    result = result.replace("{{git_branch}}", "$(git rev-parse --abbrev-ref HEAD)");
    result
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_plain() {
        assert_eq!(shell_quote("hello"), "'hello'");
    }

    #[test]
    fn shell_quote_with_spaces() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }

    #[test]
    fn shell_quote_with_apostrophe() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn resolve_vars_no_placeholders() {
        let vars = HashMap::new();
        let result = resolve_vars("just a plain string", &vars);
        assert_eq!(result, "just a plain string");
    }

    #[test]
    fn wait_for_default_timeout() {
        let wf: WaitFor = serde_json::from_str(r#"{"pattern": "ready"}"#).unwrap();
        assert_eq!(wf.timeout, 30);
        assert_eq!(wf.pattern, "ready");
    }

    #[test]
    fn resolve_vars_user_defined() {
        let mut vars = HashMap::new();
        vars.insert("project".to_string(), "/home/user/myproject".to_string());
        let result = resolve_vars("cd {{project}}", &vars);
        assert_eq!(result, "cd /home/user/myproject");
    }

    #[test]
    fn resolve_vars_builtin_cwd() {
        let vars = HashMap::new();
        let result = resolve_vars("cd {{cwd}}", &vars);
        assert_eq!(result, "cd $PWD");
    }

    #[test]
    fn resolve_vars_builtin_date() {
        let vars = HashMap::new();
        let result = resolve_vars("echo {{date}}", &vars);
        assert_eq!(result, "echo $(date +%Y-%m-%d)");
    }

    #[test]
    fn resolve_vars_builtin_git_branch() {
        let vars = HashMap::new();
        let result = resolve_vars("echo {{git_branch}}", &vars);
        assert_eq!(result, "echo $(git rev-parse --abbrev-ref HEAD)");
    }
}
