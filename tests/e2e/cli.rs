//! End-to-end tests that drive the compiled binary the way a user would.
//!
//! These cover argument parsing, subcommand wiring, exit codes, and the
//! JSON/TOML/YAML loading paths - surface area the in-crate unit tests cannot
//! reach. They are hermetic: no test starts or probes a real tmux server, so
//! they run identically in the Nix sandbox.

use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

/// Path to the binary under test, injected by Cargo for integration tests.
const BIN: &str = env!("CARGO_BIN_EXE_nix-tmux-define");

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("failed to spawn binary")
}

fn run_with_path(args: &[&str], path: &Path) -> Output {
    Command::new(BIN)
        .args(args)
        .env("PATH", path)
        .output()
        .expect("failed to spawn binary")
}

fn write_config(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create config");
    f.write_all(contents.as_bytes()).expect("write config");
    path
}

#[cfg(unix)]
fn write_executable(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create executable");
    f.write_all(contents.as_bytes()).expect("write executable");
    let mut perms = f.metadata().expect("executable metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod executable");
    path
}

const VALID_JSON: &str = r#"{
  "name": "demo",
  "root": "/tmp",
  "windows": [
    {
      "name": "main",
      "layout": {
        "type": "split", "direction": "horizontal", "ratio": 0.6,
        "first":  { "type": "pane", "command": "nvim .", "focus": true },
        "second": { "type": "pane", "command": "git status" }
      }
    }
  ]
}"#;

const VALID_TOML: &str = r#"name = "demo-toml"
root = "/tmp"
[[windows]]
name = "main"
[windows.layout]
type = "pane"
command = "echo hi"
"#;

const VALID_YAML: &str = "name: demo-yaml\nroot: /tmp\nwindows:\n  - name: main\n    layout:\n      type: pane\n      command: echo hi\n";

// ── --version / --help ───────────────────────────────────────────────────────

#[test]
fn version_succeeds() {
    let out = run(&["--version"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("nix-tmux-define"), "stdout: {stdout}");
}

#[test]
fn help_succeeds_and_lists_subcommands() {
    let out = run(&["--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for cmd in [
        "run",
        "print",
        "validate",
        "reload",
        "list",
        "schema",
        "completions",
    ] {
        assert!(
            stdout.contains(cmd),
            "help should mention `{cmd}`:\n{stdout}"
        );
    }
}

#[test]
fn no_args_fails() {
    let out = run(&[]);
    assert!(!out.status.success(), "missing subcommand must be an error");
}

// ── schema ───────────────────────────────────────────────────────────────────

#[test]
fn schema_emits_valid_json_schema() {
    let out = run(&["schema"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("schema output must be valid JSON");
    // It should look like a JSON Schema describing the Session type.
    assert!(value.get("properties").is_some() || value.get("$ref").is_some());
    assert!(stdout.contains("windows"), "schema should mention windows");
}

// ── print ────────────────────────────────────────────────────────────────────

#[test]
fn print_generates_runnable_script() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "s.json", VALID_JSON);
    let out = run(&["print", "--config", cfg.to_str().unwrap()]);
    assert!(out.status.success());
    let script = String::from_utf8_lossy(&out.stdout);
    assert!(script.starts_with("#!/usr/bin/env bash"));
    assert!(script.contains("set -euo pipefail"));
    assert!(script.contains("tmux new-session"));
    assert!(script.contains("split-window"));
    // Commands from both panes must appear.
    assert!(script.contains("'nvim .'"));
    assert!(script.contains("'git status'"));
}

#[test]
fn print_missing_file_fails() {
    let out = run(&["print", "--config", "/no/such/file.json"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("cannot read"),
        "stderr: {stderr}"
    );
}

// ── validate ─────────────────────────────────────────────────────────────────

#[test]
fn validate_accepts_each_format() {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("s.json", VALID_JSON),
        ("s.toml", VALID_TOML),
        ("s.yaml", VALID_YAML),
    ] {
        let cfg = write_config(dir.path(), name, body);
        let out = run(&["validate", "--config", cfg.to_str().unwrap()]);
        assert!(out.status.success(), "validate should accept {name}");
        // The success summary is printed to stderr.
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("window(s)"), "stderr: {stderr}");
    }
}

#[test]
fn validate_rejects_empty_windows() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "empty.json", r#"{"name":"x","windows":[]}"#);
    let out = run(&["validate", "--config", cfg.to_str().unwrap()]);
    assert!(!out.status.success(), "empty windows must fail validation");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("at least one window"), "stderr: {stderr}");
}

#[test]
fn validate_rejects_out_of_range_ratio() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"{"name":"x","windows":[{"name":"w","layout":{"type":"split","direction":"horizontal","ratio":1.5,"first":{"type":"pane"},"second":{"type":"pane"}}}]}"#;
    let cfg = write_config(dir.path(), "ratio.json", body);
    let out = run(&["validate", "--config", cfg.to_str().unwrap()]);
    assert!(!out.status.success(), "ratio 1.5 must fail validation");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("between"), "stderr: {stderr}");
}

#[test]
fn validate_rejects_injection_in_session_name() {
    let dir = tempfile::tempdir().unwrap();
    // A ':' is a tmux target separator and must be rejected.
    let cfg = write_config(
        dir.path(),
        "bad.json",
        r#"{"name":"a:b","windows":[{"name":"w","layout":{"type":"pane"}}]}"#,
    );
    let out = run(&["validate", "--config", cfg.to_str().unwrap()]);
    assert!(!out.status.success());
}

// ── list ─────────────────────────────────────────────────────────────────────

#[test]
fn list_prints_session_summary() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "s.json", VALID_JSON);
    let out = run(&["list", "--config", cfg.to_str().unwrap()]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("demo"), "stdout: {stdout}");
    assert!(stdout.contains("window(s)"), "stdout: {stdout}");
}

#[test]
fn list_does_not_probe_tmux_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "s.json", VALID_JSON);
    let out = run_with_path(&["list", "--config", cfg.to_str().unwrap()], bin_dir.path());
    assert!(
        out.status.success(),
        "list must not require tmux on PATH: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("demo"), "stdout: {stdout}");
    assert!(!stdout.contains("[running]"), "stdout: {stdout}");
}

#[cfg(unix)]
#[test]
fn list_running_status_marks_sessions_reported_by_tmux() {
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "s.json", VALID_JSON);
    write_executable(
        bin_dir.path(),
        "tmux",
        r#"#!/bin/sh
if [ "$1" = "list-sessions" ]; then
  printf 'demo\nother\n'
  exit 0
fi
printf 'unexpected tmux invocation: %s\n' "$*" >&2
exit 64
"#,
    );

    let out = run_with_path(
        &[
            "list",
            "--running-status",
            "--config",
            cfg.to_str().unwrap(),
        ],
        bin_dir.path(),
    );
    assert!(
        out.status.success(),
        "list --running-status should accept fake tmux output: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("demo"), "stdout: {stdout}");
    assert!(stdout.contains("[running]"), "stdout: {stdout}");
}

#[test]
fn run_rejects_removed_kill_server_flag_before_tmux() {
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "s.json", VALID_JSON);
    let out = run_with_path(
        &["run", "--config", cfg.to_str().unwrap(), "--kill-server"],
        bin_dir.path(),
    );
    assert!(!out.status.success(), "removed flag must be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--kill-server"), "stderr: {stderr}");
}

#[cfg(unix)]
#[test]
fn reload_dispatches_to_tmux_path_after_loading_config() {
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "s.json", VALID_JSON);
    write_executable(
        bin_dir.path(),
        "tmux",
        r#"#!/bin/sh
printf 'fake tmux reached: %s\n' "$*" >&2
exit 77
"#,
    );
    let out = run_with_path(
        &["reload", "--config", cfg.to_str().unwrap()],
        bin_dir.path(),
    );

    assert!(
        !out.status.success(),
        "reload should surface fake tmux failure"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("fake tmux reached"),
        "reload should reach the tmux-backed execution path: {stderr}"
    );
}

// ── completions ──────────────────────────────────────────────────────────────

#[test]
fn completions_emit_for_each_shell() {
    for shell in ["bash", "zsh", "fish"] {
        let out = run(&["completions", shell]);
        assert!(out.status.success(), "completions {shell} should succeed");
        assert!(
            !out.stdout.is_empty(),
            "completions {shell} produced no output"
        );
    }
}
