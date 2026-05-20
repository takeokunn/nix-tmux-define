use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Validators ──────────────────────────────────────────────────────────────

fn deserialize_env_key<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let s = String::deserialize(deserializer)?;
    if s.is_empty() {
        return Err(D::Error::custom("env var key must not be empty"));
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(D::Error::custom(format!(
            "invalid env var key {s:?}: must start with ASCII letter or underscore"
        )));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(D::Error::custom(format!(
            "invalid env var key {s:?}: must contain only ASCII letters, digits, or underscores"
        )));
    }
    Ok(s)
}

fn deserialize_tmux_name<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let s = String::deserialize(deserializer)?;
    if s.is_empty() {
        return Err(D::Error::custom("tmux name must not be empty"));
    }
    // ':' separates session:window and '.' separates window.pane in tmux targets
    if let Some(ch) = s.chars().find(|&c| c == ':' || c == '.') {
        return Err(D::Error::custom(format!(
            "invalid tmux name {s:?}: must not contain {ch:?} (tmux target separator)"
        )));
    }
    Ok(s)
}

// ─── Model ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct Session {
    #[serde(deserialize_with = "deserialize_tmux_name")]
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
    #[serde(deserialize_with = "deserialize_tmux_name")]
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
    #[serde(deserialize_with = "deserialize_env_key")]
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

    // ── EnvVar key validation ─────────────────────────────────────────────────

    fn parse_env_key(key: &str) -> Result<EnvVar, serde_json::Error> {
        serde_json::from_str(&format!(r#"{{"key":"{key}","value":"v"}}"#))
    }

    #[test]
    fn env_key_valid_simple() {
        let ev = parse_env_key("FOO_BAR").unwrap();
        assert_eq!(ev.key, "FOO_BAR");
    }

    #[test]
    fn env_key_valid_starts_with_underscore() {
        let ev = parse_env_key("_MY_VAR").unwrap();
        assert_eq!(ev.key, "_MY_VAR");
    }

    #[test]
    fn env_key_valid_lowercase() {
        assert!(parse_env_key("my_var").is_ok());
    }

    #[test]
    fn env_key_invalid_starts_with_digit() {
        let err = parse_env_key("1FOO").unwrap_err();
        assert!(err.to_string().contains("must start with"), "{err}");
    }

    #[test]
    fn env_key_invalid_contains_semicolon() {
        let err = parse_env_key("FOO;rm -rf /").unwrap_err();
        assert!(err.to_string().contains("must contain only"), "{err}");
    }

    #[test]
    fn env_key_invalid_contains_dollar() {
        assert!(parse_env_key("FOO$BAR").is_err());
    }

    #[test]
    fn env_key_invalid_empty() {
        assert!(parse_env_key("").is_err());
    }

    // ── tmux name validation ──────────────────────────────────────────────────

    fn parse_session_name(name: &str) -> Result<Session, serde_json::Error> {
        serde_json::from_str(&format!(
            r#"{{"name":"{name}","windows":[{{"name":"w","layout":{{"type":"pane"}}}}]}}"#
        ))
    }

    fn parse_window_name(name: &str) -> Result<Session, serde_json::Error> {
        serde_json::from_str(&format!(
            r#"{{"name":"s","windows":[{{"name":"{name}","layout":{{"type":"pane"}}}}]}}"#
        ))
    }

    #[test]
    fn session_name_valid() {
        assert!(parse_session_name("my-session").is_ok());
    }

    #[test]
    fn session_name_invalid_colon() {
        let err = parse_session_name("my:session").unwrap_err();
        assert!(err.to_string().contains("':'"), "{err}");
    }

    #[test]
    fn session_name_invalid_empty() {
        assert!(parse_session_name("").is_err());
    }

    #[test]
    fn window_name_valid() {
        assert!(parse_window_name("editor").is_ok());
    }

    #[test]
    fn window_name_invalid_colon() {
        let err = parse_window_name("win:one").unwrap_err();
        assert!(err.to_string().contains("':'"), "{err}");
    }

    #[test]
    fn session_name_invalid_dot() {
        let err = parse_session_name("my.session").unwrap_err();
        assert!(err.to_string().contains("'.'"), "{err}");
    }

    #[test]
    fn window_name_invalid_dot() {
        assert!(parse_window_name("win.sub").is_err());
    }

    #[test]
    fn session_name_allows_hyphen_and_space() {
        assert!(parse_session_name("my-session").is_ok());
        assert!(parse_session_name("my session").is_ok());
    }

    #[test]
    fn env_key_invalid_hyphen() {
        assert!(parse_env_key("FOO-BAR").is_err());
    }

    #[test]
    fn env_key_invalid_dot() {
        assert!(parse_env_key("FOO.BAR").is_err());
    }

    #[test]
    fn env_key_invalid_space() {
        assert!(parse_env_key("FOO BAR").is_err());
    }

    #[test]
    fn env_key_single_underscore_is_valid() {
        assert!(parse_env_key("_").is_ok());
    }
}
