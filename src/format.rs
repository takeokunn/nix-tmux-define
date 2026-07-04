use crate::model::Session;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Load a session from a file, detecting format by extension.
///
/// - `.toml` → TOML
/// - `.yaml` / `.yml` → YAML
/// - anything else → JSON
pub fn load_session(path: &Path) -> Result<Session> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read '{}'", path.display()))?;
    // Strip UTF-8 BOM (U+FEFF) that Windows editors sometimes prepend.
    // Without this, serde_json/serde_yaml produce misleading parse errors.
    let raw = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
    let session: Session = match path.extension().and_then(|e| e.to_str()) {
        Some("toml") => {
            toml::from_str(raw).with_context(|| format!("invalid TOML in '{}'", path.display()))?
        }
        Some("yaml") | Some("yml") => serde_yaml::from_str(raw)
            .with_context(|| format!("invalid YAML in '{}'", path.display()))?,
        _ => serde_json::from_str(raw)
            .with_context(|| format!("invalid JSON in '{}'", path.display()))?,
    };
    session.validate()?;
    Ok(session)
}

/// A directory config file that could not be loaded as a session, together with
/// a concise, single-line reason. Produced by [`load_sessions_from_dir_lenient`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedConfig {
    pub path: PathBuf,
    pub reason: String,
}

/// Result of a lenient directory scan: the sessions that loaded successfully,
/// plus the config-extension files that were skipped and why.
#[derive(Debug, Default)]
pub struct LenientDirScan {
    pub sessions: Vec<Session>,
    pub skipped: Vec<SkippedConfig>,
}

/// Collects the config-extension files (`.json`, `.toml`, `.yaml`, `.yml`) that
/// live directly in `dir`, sorted by path for deterministic ordering.
///
/// Extension-less files (e.g. `config`) are intentionally ignored even though
/// `load_session()` would attempt JSON parsing for them; use `load_session()`
/// directly when you need to load a file without a recognised extension.
fn config_paths_in_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read directory '{}'", dir.display()))?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("cannot read entry in directory '{}'", dir.display()))?;
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if matches!(ext, "json" | "toml" | "yaml" | "yml") {
                    paths.push(path);
                }
            }
        }
    }
    // read_dir yields entries in an unspecified order; sort so both the loaded
    // sessions and any skip warnings are reported deterministically.
    paths.sort();
    Ok(paths)
}

/// Distils an error chain into a single-line reason suitable for a warning.
fn skip_reason(err: &anyhow::Error) -> String {
    err.root_cause()
        .to_string()
        .lines()
        .next()
        .unwrap_or("could not be parsed as a session config")
        .trim()
        .to_owned()
}

/// Load all supported session configs from a directory, failing on the first
/// file that does not parse and validate as a [`Session`].
///
/// Only `.json`, `.toml`, `.yaml`, and `.yml` files are considered.
/// The returned list is sorted by session name.
///
/// Use this for an explicitly requested directory (e.g. `list --config-dir`),
/// where a malformed config is an error the caller wants surfaced. For a
/// best-effort scan of a directory that may hold unrelated config files, use
/// [`load_sessions_from_dir_lenient`].
pub fn load_sessions_from_dir(dir: &Path) -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    for path in config_paths_in_dir(dir)? {
        let session = load_session(&path)
            .with_context(|| format!("failed to load session config '{}'", path.display()))?;
        sessions.push(session);
    }
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

/// Like [`load_sessions_from_dir`], but instead of failing on the first config
/// file that cannot be loaded as a session, it skips that file and records a
/// concise reason in [`LenientDirScan::skipped`].
///
/// This is used for the implicit current-directory scan of `list`, where
/// unrelated config-extension files (`Cargo.toml`, `package.json`,
/// `tsconfig.json`, …) are expected to be present and must not abort the whole
/// command. Reading the directory itself still errors (e.g. it does not exist).
/// Sessions are sorted by name; skips are sorted by path.
pub fn load_sessions_from_dir_lenient(dir: &Path) -> Result<LenientDirScan> {
    let mut sessions = Vec::new();
    let mut skipped = Vec::new();
    for path in config_paths_in_dir(dir)? {
        match load_session(&path) {
            Ok(session) => sessions.push(session),
            Err(err) => skipped.push(SkippedConfig {
                path,
                reason: skip_reason(&err),
            }),
        }
    }
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(LenientDirScan { sessions, skipped })
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
    fn load_json_with_utf8_bom() {
        let f = write_temp(".json", &format!("\u{FEFF}{}", json_content()));
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
    fn load_sessions_from_dir_rejects_invalid_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.json"), "{ invalid }").unwrap();
        std::fs::write(dir.path().join("good.json"), json_content()).unwrap();

        let err = load_sessions_from_dir(dir.path()).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("bad.json"),
            "error should identify the invalid config file: {message}"
        );
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
    fn load_sessions_from_dir_lenient_skips_non_session_configs() {
        let dir = tempfile::tempdir().unwrap();
        // A real session config…
        std::fs::write(dir.path().join("dev.json"), json_content()).unwrap();
        // …a Cargo.toml-style file (valid TOML, not a session)…
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        // …and a syntactically broken JSON file.
        std::fs::write(dir.path().join("broken.json"), "{ not valid").unwrap();

        let scan = load_sessions_from_dir_lenient(dir.path()).unwrap();

        assert_eq!(scan.sessions.len(), 1, "only the real session should load");
        assert_eq!(scan.sessions[0].name, "test-session");

        let skipped: Vec<_> = scan
            .skipped
            .iter()
            .map(|s| s.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(skipped.contains(&"Cargo.toml".to_owned()), "{skipped:?}");
        assert!(skipped.contains(&"broken.json".to_owned()), "{skipped:?}");
        assert!(
            scan.skipped.iter().all(|s| !s.reason.is_empty()),
            "every skip must carry a non-empty single-line reason: {:?}",
            scan.skipped
        );
        assert!(
            scan.skipped.iter().all(|s| !s.reason.contains('\n')),
            "skip reasons must be single-line for a clean warning: {:?}",
            scan.skipped
        );
    }

    #[test]
    fn load_sessions_from_dir_lenient_reports_no_skips_when_all_valid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.json"), json_content()).unwrap();
        std::fs::write(dir.path().join("b.yaml"), yaml_content()).unwrap();

        let scan = load_sessions_from_dir_lenient(dir.path()).unwrap();
        assert_eq!(scan.sessions.len(), 2);
        assert!(scan.skipped.is_empty(), "{:?}", scan.skipped);
    }

    #[test]
    fn load_sessions_from_dir_lenient_still_errors_on_missing_directory() {
        let result = load_sessions_from_dir_lenient(Path::new("/nonexistent/directory"));
        assert!(
            result.is_err(),
            "reading a missing directory must still error"
        );
    }

    #[test]
    fn load_sessions_from_dir_nonexistent_returns_err() {
        let result = load_sessions_from_dir(Path::new("/nonexistent/directory"));
        assert!(result.is_err());
    }

    #[test]
    fn load_session_accepts_dynamic_builtin_in_session_root() {
        let f = write_temp(
            ".json",
            r#"{"name":"s","root":"{{cwd}}","windows":[{"name":"w","layout":{"type":"pane"}}]}"#,
        );
        let session = load_session(f.path()).unwrap();
        assert_eq!(session.root.as_deref(), Some("{{cwd}}"));
    }

    #[test]
    fn load_session_accepts_dynamic_builtin_in_window_root() {
        let f = write_temp(
            ".json",
            r#"{"name":"s","windows":[{"name":"w","root":"{{git_branch}}","layout":{"type":"pane"}}]}"#,
        );
        let session = load_session(f.path()).unwrap();
        assert_eq!(session.windows[0].root.as_deref(), Some("{{git_branch}}"));
    }

    #[test]
    fn load_session_allows_static_root() {
        let f = write_temp(".json", json_content());
        assert!(load_session(f.path()).is_ok());
    }
}
