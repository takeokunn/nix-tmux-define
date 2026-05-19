use crate::model::Session;
use anyhow::{Context, Result};
use std::path::Path;

/// Load a session from a file, detecting format by extension.
///
/// - `.toml` → TOML
/// - `.yaml` / `.yml` → YAML
/// - anything else → JSON
pub fn load_session(path: &Path) -> Result<Session> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read '{}'", path.display()))?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("toml") => toml::from_str(&raw)
            .with_context(|| format!("invalid TOML in '{}'", path.display())),
        Some("yaml") | Some("yml") => serde_yaml::from_str(&raw)
            .with_context(|| format!("invalid YAML in '{}'", path.display())),
        _ => serde_json::from_str(&raw)
            .with_context(|| format!("invalid JSON in '{}'", path.display())),
    }
}

/// Load all sessions from a directory, silently skipping files with errors.
///
/// Only `.json`, `.toml`, `.yaml`, and `.yml` files are considered.
/// The returned list is sorted by session name.
pub fn load_sessions_from_dir(dir: &Path) -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read directory '{}'", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if matches!(ext, "json" | "toml" | "yaml" | "yml") {
                    match load_session(&path) {
                        Ok(s) => sessions.push(s),
                        Err(e) => eprintln!("warning: skipping '{}': {}", path.display(), e),
                    }
                }
            }
        }
    }
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn json_content() -> &'static str {
        r#"{
            "name": "test-session",
            "windows": [{"name": "main", "layout": {"type": "pane"}}]
        }"#
    }

    fn toml_content() -> &'static str {
        r#"name = "test-session"
[[windows]]
name = "main"
[windows.layout]
type = "pane"
"#
    }

    fn yaml_content() -> &'static str {
        "name: test-session\nwindows:\n  - name: main\n    layout:\n      type: pane\n"
    }

    fn write_temp(ext: &str, content: &str) -> NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(ext).tempfile().unwrap();
        write!(f, "{}", content).unwrap();
        f
    }

    #[test]
    fn load_json() {
        let f = write_temp(".json", json_content());
        let s = load_session(f.path()).unwrap();
        assert_eq!(s.name, "test-session");
        assert_eq!(s.windows.len(), 1);
    }

    #[test]
    fn load_toml() {
        let f = write_temp(".toml", toml_content());
        let s = load_session(f.path()).unwrap();
        assert_eq!(s.name, "test-session");
        assert_eq!(s.windows.len(), 1);
    }

    #[test]
    fn load_yaml() {
        let f = write_temp(".yaml", yaml_content());
        let s = load_session(f.path()).unwrap();
        assert_eq!(s.name, "test-session");
        assert_eq!(s.windows.len(), 1);
    }

    #[test]
    fn load_unknown_ext_defaults_json() {
        let f = write_temp(".txt", json_content());
        let s = load_session(f.path()).unwrap();
        assert_eq!(s.name, "test-session");
    }

    #[test]
    fn load_nonexistent_returns_err() {
        let result = load_session(Path::new("/nonexistent/path/file.json"));
        assert!(result.is_err());
    }
}
