//! Differential property tests: the compiler (`print`) and executor (`run`)
//! paths must build **structurally identical** pane layouts for any session.
//!
//! Both paths now render the same [`LayoutPlan`], so the plan is used as the
//! oracle: each path is independently checked to render the plan faithfully
//! (same ordered splits, commands, titles, and focus selection). If both equal
//! the oracle, they equal each other — which is exactly the invariant whose past
//! violations were the shipped compiler/executor divergence bugs.

use nix_tmux_define::{
    Compiler, Direction, Executor, LayoutNode, LayoutPlan, PaneCommand, PaneTitle,
    RecordingBackend, RootTemplate, Session, SplitRatio, TmuxName, Window,
};
use proptest::prelude::*;
use std::collections::BTreeMap;

/// Naming-independent fingerprint of a whole session's build.
#[derive(Debug, Default, PartialEq, Eq)]
struct SessionShape {
    /// `(flag, pct)` for every `split-window`, in execution order.
    splits: Vec<(String, u32)>,
    /// Resolved pane commands, in phase-2 order.
    commands: Vec<String>,
    /// Resolved pane titles, in phase-2 order.
    titles: Vec<String>,
    /// Number of focus `select-pane` calls (one per window that focuses a pane).
    focus_count: usize,
}

/// The oracle: what both paths *should* render, derived straight from the plan.
fn plan_shape(session: &Session) -> SessionShape {
    let mut shape = SessionShape::default();
    for window in &session.windows {
        let plan = LayoutPlan::build(&window.layout, &session.vars).unwrap();
        for split in plan.splits() {
            shape
                .splits
                .push((split.flag.as_str().to_owned(), split.pct.as_u32()));
        }
        let mut window_focuses = false;
        for leaf in plan.leaves() {
            if let Some(cmd) = &leaf.command {
                shape.commands.push(cmd.as_str().to_owned());
            }
            if let Some(title) = &leaf.title {
                shape.titles.push(title.as_str().to_owned());
            }
            window_focuses |= leaf.focus;
        }
        if window_focuses {
            shape.focus_count += 1;
        }
    }
    shape
}

/// Extract the fingerprint from the executor's recorded backend calls.
fn executor_shape(session: &Session) -> SessionShape {
    let backend = RecordingBackend::new();
    Executor::new(&backend).run(session).unwrap();

    let mut shape = SessionShape::default();
    for call in backend.calls() {
        if let Some(rest) = call.strip_prefix("split-window:") {
            // %parent:-h:40:/tmp:%new[:env:...]
            let parts: Vec<&str> = rest.split(':').collect();
            let flag = parts[1].to_owned();
            let pct: u32 = parts[2].parse().unwrap();
            shape.splits.push((flag, pct));
        } else if let Some(rest) = call.strip_prefix("send-keys:") {
            // %id:command  → command is everything after the pane id
            let command = rest.split_once(':').unwrap().1.to_owned();
            shape.commands.push(command);
        } else if let Some(rest) = call.strip_prefix("set-pane-title:") {
            let title = rest.split_once(':').unwrap().1.to_owned();
            shape.titles.push(title);
        } else if call.starts_with("select-pane:") {
            shape.focus_count += 1;
        }
    }
    shape
}

/// Extract the fingerprint from the compiler's generated bash script.
fn compiler_shape(session: &Session) -> SessionShape {
    let mut compiler = Compiler::new();
    compiler.compile(session).unwrap();
    let script = compiler.into_script();

    let mut shape = SessionShape::default();
    for line in script.lines() {
        let line = line.trim();
        if line.contains("tmux split-window") {
            let flag = if line.contains(" -h ") { "-h" } else { "-v" }.to_owned();
            let pct = parse_pct(line).expect("split line must carry -l N%");
            shape.splits.push((flag, pct));
        } else if line.contains("tmux send-keys") && line.contains(" -l -- ") {
            let quoted = line.split(" -l -- ").nth(1).unwrap();
            shape.commands.push(unquote(quoted));
        } else if line.contains("tmux select-pane") && line.contains(" -T ") {
            let quoted = line.split(" -T ").nth(1).unwrap();
            shape.titles.push(unquote(quoted));
        } else if line.contains("tmux select-pane") {
            // select-pane without -T is the focus selection
            shape.focus_count += 1;
        }
    }
    shape
}

/// Parse the `-l NN%` pane size out of a compiled `split-window` line.
fn parse_pct(line: &str) -> Option<u32> {
    let after = line.split("-l ").nth(1)?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Strip one layer of POSIX single-quoting. The generator only emits commands
/// and titles without single quotes, so `shell_quote` wraps them in `'…'` with
/// no interior escaping — undone here for comparison with the executor.
fn unquote(s: &str) -> String {
    s.strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(s)
        .to_owned()
}

// ── Strategies ────────────────────────────────────────────────────────────────

fn arb_opt_command() -> impl Strategy<Value = Option<PaneCommand>> {
    prop_oneof![
        1 => Just(None),
        3 => "[a-z][a-z0-9 ]{0,8}".prop_map(|s| Some(PaneCommand::new(s).unwrap())),
    ]
}

fn arb_opt_title() -> impl Strategy<Value = Option<PaneTitle>> {
    prop_oneof![
        2 => Just(None),
        1 => "[a-z][a-z0-9]{0,6}".prop_map(|s| Some(PaneTitle::new(s).unwrap())),
    ]
}

fn arb_layout() -> impl Strategy<Value = LayoutNode> {
    let leaf =
        (arb_opt_command(), any::<bool>(), arb_opt_title()).prop_map(|(command, focus, title)| {
            LayoutNode::Pane {
                command,
                focus,
                title,
                // wait_for is intentionally excluded: the executor's wait loop would
                // sleep against a RecordingBackend that never emits the pattern.
                wait_for: None,
            }
        });
    leaf.prop_recursive(4, 40, 3, |inner| {
        (
            prop_oneof![Just(Direction::Horizontal), Just(Direction::Vertical)],
            6u32..=98u32,
            inner.clone(),
            inner,
        )
            .prop_map(|(dir, pct, first, second)| {
                LayoutNode::split(
                    dir,
                    SplitRatio::new(f64::from(pct) / 100.0).unwrap(),
                    first,
                    second,
                )
            })
    })
}

fn arb_session() -> impl Strategy<Value = Session> {
    proptest::collection::vec(arb_layout(), 1..=3).prop_map(|layouts| {
        let windows: Vec<Window> = layouts
            .into_iter()
            .enumerate()
            .map(|(i, layout)| Window {
                name: TmuxName::new(format!("w{i}")).unwrap(),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
                layout,
            })
            .collect();
        Session {
            name: TmuxName::new("s").unwrap(),
            root: Some(RootTemplate::new("/tmp").unwrap()),
            windows,
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The executor renders the plan faithfully.
    #[test]
    fn executor_matches_plan(session in arb_session()) {
        prop_assert_eq!(executor_shape(&session), plan_shape(&session));
    }

    /// The compiler renders the plan faithfully.
    #[test]
    fn compiler_matches_plan(session in arb_session()) {
        prop_assert_eq!(compiler_shape(&session), plan_shape(&session));
    }

    /// Transitively, the two paths agree with each other on structure.
    #[test]
    fn compiler_and_executor_agree(session in arb_session()) {
        prop_assert_eq!(compiler_shape(&session), executor_shape(&session));
    }
}
