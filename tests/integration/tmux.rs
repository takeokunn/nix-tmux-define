//! Integration tests for the executor/backend contract.
//!
//! These deliberately use `RecordingBackend`; no test in this crate shells out
//! to the real `tmux` binary.

use std::collections::BTreeMap;

use nix_tmux_define::{
    Direction, EnvVar, EnvVarName, EnvVarValue, Executor, LayoutNode, PaneCommand, PaneTitle,
    RecordingBackend, RootTemplate, Session, SplitRatio, TemplateVarName, TemplateVarValue,
    TmuxLayoutPreset, TmuxName, TmuxOptionName, TmuxOptionValue, WaitFor, WaitPattern,
    WaitTimeoutSeconds, Window,
};

fn ratio(value: f64) -> SplitRatio {
    SplitRatio::new(value).unwrap()
}

fn pane_command(value: &str) -> PaneCommand {
    PaneCommand::new(value).unwrap()
}

fn pane_title(value: &str) -> PaneTitle {
    PaneTitle::new(value).unwrap()
}

fn wait_pattern(value: &str) -> WaitPattern {
    WaitPattern::new(value).unwrap()
}

fn tmux_name(value: &str) -> TmuxName {
    TmuxName::new(value).unwrap()
}

fn root_template(value: &str) -> RootTemplate {
    RootTemplate::new(value).unwrap()
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

fn option_map(pairs: &[(&str, &str)]) -> BTreeMap<TmuxOptionName, TmuxOptionValue> {
    pairs
        .iter()
        .map(|(key, value)| (option_name(key), option_value(value)))
        .collect()
}

fn assert_call(calls: &[String], expected: &str) {
    assert!(
        calls.iter().any(|call| call == expected),
        "missing call {expected:?}; calls: {calls:#?}"
    );
}

fn mock_session() -> Session {
    Session {
        name: tmux_name("demo"),
        root: Some(root_template("/workspace/{{project}}")),
        windows: vec![Window {
            name: tmux_name("main"),
            root: Some(root_template("/workspace/{{project}}/app")),
            env: vec![EnvVar {
                key: env_key("WINDOW"),
                value: env_value("2"),
            }],
            options: option_map(&[("synchronize-panes", "on")]),
            select_layout: Some(layout_preset("tiled")),
            layout: LayoutNode::Split {
                direction: Direction::Horizontal,
                ratio: ratio(0.60),
                first: Box::new(LayoutNode::Pane {
                    command: Some(pane_command("echo {{project}}")),
                    focus: true,
                    title: Some(pane_title("editor")),
                    wait_for: None,
                }),
                second: Box::new(LayoutNode::Pane {
                    command: Some(pane_command("echo second")),
                    focus: false,
                    title: None,
                    wait_for: Some(WaitFor {
                        pattern: wait_pattern("ready"),
                        timeout: WaitTimeoutSeconds::new(1).unwrap(),
                    }),
                }),
            },
        }],
        env: vec![EnvVar {
            key: env_key("GLOBAL"),
            value: env_value("1"),
        }],
        pre_hook: None,
        options: option_map(&[("status", "off")]),
        vars: [(var_key("project"), var_value("demo"))]
            .into_iter()
            .collect(),
    }
}

#[test]
fn executor_run_uses_recording_backend_without_spawning_tmux() {
    let backend = RecordingBackend::new();
    *backend.capture_output.borrow_mut() = "ready".to_owned();

    Executor::new(&backend).run(&mock_session()).unwrap();

    let calls = backend.calls();
    assert_eq!(calls.first().map(String::as_str), Some("has-session:demo"));
    assert_call(
        &calls,
        "new-session:demo:/workspace/demo/app:main:%0:env:GLOBAL=1,WINDOW=2",
    );
    assert_call(
        &calls,
        "split-window:%0:-h:40:/workspace/demo/app:%1:env:GLOBAL=1,WINDOW=2",
    );
    assert_call(&calls, "send-keys:%0:echo demo");
    assert_call(&calls, "send-keys:%1:echo second");
    assert_call(&calls, "capture-pane:%1");
    assert_call(&calls, "set-pane-title:%0:editor");
    assert_call(&calls, "set-option:demo:status:off");
    assert_call(&calls, "set-window-option:demo:main:synchronize-panes:on");
    assert_call(&calls, "select-layout:demo:main:tiled");
    assert_call(&calls, "select-pane:%0");
    assert_call(&calls, "select-window:demo:main");
    assert_eq!(
        calls.last().map(String::as_str),
        Some("attach-or-switch:demo")
    );
}

#[test]
fn executor_reload_replaces_existing_session_with_recording_backend() {
    let backend = RecordingBackend::new();
    backend.session_exists.set(true);
    *backend.capture_output.borrow_mut() = "ready".to_owned();

    Executor::new(&backend).reload(&mock_session()).unwrap();

    let calls = backend.calls();
    let pid = std::process::id();
    let new_name = format!("demo__ntd_reload_new_{pid}");
    let old_name = format!("demo__ntd_reload_old_{pid}");

    assert_eq!(calls.first().map(String::as_str), Some("has-session:demo"));
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with(&format!("new-session:{new_name}:"))),
        "replacement session must be built before the swap: {calls:#?}"
    );
    assert_call(&calls, &format!("rename-session:demo:{old_name}"));
    assert_call(&calls, &format!("rename-session:{new_name}:demo"));
    assert_call(&calls, &format!("kill-session:{old_name}"));
    assert_call(&calls, "select-window:demo:main");
    assert_eq!(
        calls.last().map(String::as_str),
        Some("attach-or-switch:demo")
    );
}
