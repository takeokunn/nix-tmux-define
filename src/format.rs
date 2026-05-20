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
        Some("toml") => {
            toml::from_str(&raw).with_context(|| format!("invalid TOML in '{}'", path.display()))
        }
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

    #[test]
    fn load_yml_extension() {
        let f = write_temp(".yml", yaml_content());
        let s = load_session(f.path()).unwrap();
        assert_eq!(s.name, "test-session");
    }

    #[test]
    fn load_invalid_json_returns_err() {
        let f = write_temp(".json", "{ not valid json }");
        assert!(load_session(f.path()).is_err());
    }

    #[test]
    fn load_invalid_toml_returns_err() {
        let f = write_temp(".toml", "not = valid = toml");
        assert!(load_session(f.path()).is_err());
    }

    #[test]
    fn load_invalid_yaml_returns_err() {
        let f = write_temp(".yaml", ": - : bad\n  yaml: [");
        assert!(load_session(f.path()).is_err());
    }

    #[test]
    fn load_sessions_from_dir_basic() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("a.json")).unwrap();
        write!(f, "{}", json_content()).unwrap();

        let sessions = load_sessions_from_dir(dir.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "test-session");
    }

    #[test]
    fn load_sessions_from_dir_sorted_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let s1 = r#"{"name":"z-session","windows":[{"name":"w","layout":{"type":"pane"}}]}"#;
        let s2 = r#"{"name":"a-session","windows":[{"name":"w","layout":{"type":"pane"}}]}"#;
        std::fs::write(dir.path().join("z.json"), s1).unwrap();
        std::fs::write(dir.path().join("a.json"), s2).unwrap();

        let sessions = load_sessions_from_dir(dir.path()).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "a-session");
        assert_eq!(sessions[1].name, "z-session");
    }

    #[test]
    fn load_sessions_from_dir_skips_invalid_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.json"), "{ invalid }").unwrap();
        std::fs::write(dir.path().join("good.json"), json_content()).unwrap();

        let sessions = load_sessions_from_dir(dir.path()).unwrap();
        assert_eq!(sessions.len(), 1, "invalid file should be silently skipped");
        assert_eq!(sessions[0].name, "test-session");
    }

    #[test]
    fn load_sessions_from_dir_ignores_non_config_extensions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("session.txt"), json_content()).unwrap();
        std::fs::write(dir.path().join("session.md"), json_content()).unwrap();
        std::fs::write(dir.path().join("session.json"), json_content()).unwrap();

        let sessions = load_sessions_from_dir(dir.path()).unwrap();
        assert_eq!(
            sessions.len(),
            1,
            "only .json/.toml/.yaml/.yml should be loaded"
        );
    }

    #[test]
    fn load_sessions_from_dir_empty() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = load_sessions_from_dir(dir.path()).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn load_sessions_from_dir_nonexistent_returns_err() {
        let result = load_sessions_from_dir(Path::new("/nonexistent/directory"));
        assert!(result.is_err());
    }
}
