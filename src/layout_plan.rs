//! A typed, backend-agnostic plan describing how to build a window's panes.
//!
//! Both execution paths — the [`Compiler`](crate::Compiler) (which emits a bash
//! script) and the [`Executor`](crate::Executor) (which drives tmux directly) —
//! used to walk the [`LayoutNode`] tree with their own near-identical recursive
//! functions. That duplication was the source of real, shipped divergence bugs
//! (env vars dropped on one path, a broken tmux target on the other).
//!
//! [`LayoutPlan`] makes the traversal the single source of truth: it resolves
//! template variables, allocates pane indices, and records the exact ordered
//! sequence of `split-window` operations (phase 1) and per-leaf configuration
//! (phase 2). Each backend then only *renders* the plan — it can no longer
//! disagree about the structure, because there is only one structure.
//!
//! Pane indices are dense and start at `0` (the window's initial pane). A
//! [`PlannedSplit`] always has `parent < new`, and splits are emitted in
//! strictly increasing `new` order, so a renderer can materialise panes by
//! pushing onto a `Vec` indexed by pane number.

use crate::model::{
    resolve_vars, LayoutNode, PaneCommand, PaneTitle, TemplateVars, TmuxPanePercent, TmuxSplitFlag,
    WaitFor,
};
use anyhow::Result;

/// Dense index into a window's panes; `0` is the window's initial pane.
pub type PaneIndex = usize;

/// A single `split-window` operation: split `parent`, producing pane `new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSplit {
    pub parent: PaneIndex,
    pub new: PaneIndex,
    pub flag: TmuxSplitFlag,
    /// Size of the *new* (second) pane, as a percentage.
    pub pct: TmuxPanePercent,
}

/// The configuration to apply to one leaf pane, in phase-2 visitation order.
///
/// `command` and `title` are already template-resolved. `wait_for` is kept as a
/// whole [`WaitFor`], so a leaf can never be "waiting" without a pattern — a
/// class of bug the previous split-field representation allowed.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedLeaf {
    pub pane: PaneIndex,
    pub command: Option<PaneCommand>,
    pub title: Option<PaneTitle>,
    pub focus: bool,
    pub wait_for: Option<WaitFor>,
}

/// A fully resolved, backend-agnostic build plan for one window's layout.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutPlan {
    /// Total number of panes; valid indices are `0..pane_count`.
    pane_count: usize,
    /// `split-window` operations in execution order (phase 1).
    splits: Vec<PlannedSplit>,
    /// Leaf-pane configuration in tree-visitation order (phase 2).
    leaves: Vec<PlannedLeaf>,
}

impl LayoutPlan {
    /// Builds the plan for a window `root` layout, resolving template variables
    /// from `vars`. Fails if the tree is nested deeper than
    /// [`MAX_LAYOUT_DEPTH`](crate::MAX_LAYOUT_DEPTH) or a resolved command/title
    /// is invalid (e.g. contains a NUL byte).
    pub fn build(root: &LayoutNode, vars: &TemplateVars) -> Result<Self> {
        let mut plan = LayoutPlan {
            pane_count: 1,
            splits: Vec::new(),
            leaves: Vec::new(),
        };
        plan.walk(root, 0, vars, 0)?;
        Ok(plan)
    }

    /// Number of panes the plan builds (initial pane included).
    pub fn pane_count(&self) -> usize {
        self.pane_count
    }

    /// The `split-window` operations, in the order they must run.
    pub fn splits(&self) -> &[PlannedSplit] {
        &self.splits
    }

    /// The leaf-pane configurations, in phase-2 visitation order.
    pub fn leaves(&self) -> &[PlannedLeaf] {
        &self.leaves
    }

    fn alloc(&mut self) -> PaneIndex {
        let idx = self.pane_count;
        self.pane_count += 1;
        idx
    }

    fn walk(
        &mut self,
        node: &LayoutNode,
        current: PaneIndex,
        vars: &TemplateVars,
        depth: usize,
    ) -> Result<()> {
        if depth > crate::MAX_LAYOUT_DEPTH {
            anyhow::bail!(
                "layout tree is too deeply nested (max depth: {})",
                crate::MAX_LAYOUT_DEPTH
            );
        }
        match node {
            LayoutNode::Pane {
                command,
                focus,
                title,
                wait_for,
            } => {
                self.leaves.push(PlannedLeaf {
                    pane: current,
                    command: command
                        .as_ref()
                        .map(|c| PaneCommand::new(resolve_vars(c.as_str(), vars)))
                        .transpose()?,
                    title: title
                        .as_ref()
                        .map(|t| PaneTitle::new(resolve_vars(t.as_str(), vars)))
                        .transpose()?,
                    focus: *focus,
                    wait_for: wait_for.clone(),
                });
            }
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let new = self.alloc();
                self.splits.push(PlannedSplit {
                    parent: current,
                    new,
                    flag: direction.tmux_split_flag(),
                    pct: ratio.tmux_second_pane_percent(),
                });
                self.walk(first, current, vars, depth + 1)?;
                self.walk(second, new, vars, depth + 1)?;
            }
        }
        Ok(())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Direction, LayoutNode, SplitRatio, WaitPattern, WaitTimeoutSeconds};
    use std::collections::BTreeMap;

    fn no_vars() -> TemplateVars {
        BTreeMap::new()
    }

    fn ratio(v: f64) -> SplitRatio {
        SplitRatio::new(v).unwrap()
    }

    #[test]
    fn single_pane_has_one_leaf_no_splits() {
        let plan = LayoutPlan::build(&LayoutNode::pane(), &no_vars()).unwrap();
        assert_eq!(plan.pane_count(), 1);
        assert!(plan.splits().is_empty());
        assert_eq!(plan.leaves().len(), 1);
        assert_eq!(plan.leaves()[0].pane, 0);
    }

    #[test]
    fn split_allocates_second_pane_and_orders_children() {
        // first stays on the parent pane (0); second lands on the new pane (1).
        let node = LayoutNode::split(
            Direction::Horizontal,
            ratio(0.6),
            LayoutNode::command("a").unwrap(),
            LayoutNode::command("b").unwrap(),
        );
        let plan = LayoutPlan::build(&node, &no_vars()).unwrap();

        assert_eq!(plan.pane_count(), 2);
        assert_eq!(plan.splits().len(), 1);
        assert_eq!(plan.splits()[0].parent, 0);
        assert_eq!(plan.splits()[0].new, 1);
        assert_eq!(plan.splits()[0].flag, TmuxSplitFlag::Horizontal);
        assert_eq!(plan.splits()[0].pct.as_u32(), 40);

        // Leaves are in visitation order: first child (pane 0) then second (pane 1).
        let panes: Vec<_> = plan.leaves().iter().map(|l| l.pane).collect();
        assert_eq!(panes, vec![0, 1]);
        assert_eq!(plan.leaves()[0].command.as_ref().unwrap().as_str(), "a");
        assert_eq!(plan.leaves()[1].command.as_ref().unwrap().as_str(), "b");
    }

    #[test]
    fn splits_are_monotonic_and_parent_precedes_new() {
        // A deeper right-leaning tree: every split's parent must already exist
        // (parent < new) and new indices must increase by one each time, so a
        // Vec-pushing renderer stays correct.
        let node = LayoutNode::split(
            Direction::Vertical,
            ratio(0.5),
            LayoutNode::pane(),
            LayoutNode::split(
                Direction::Horizontal,
                ratio(0.5),
                LayoutNode::pane(),
                LayoutNode::pane(),
            ),
        );
        let plan = LayoutPlan::build(&node, &no_vars()).unwrap();
        assert_eq!(plan.pane_count(), 3);
        for (i, split) in plan.splits().iter().enumerate() {
            assert!(
                split.parent < split.new,
                "parent must precede new: {split:?}"
            );
            assert_eq!(split.new, i + 1, "new indices must be dense and monotonic");
        }
    }

    #[test]
    fn resolves_template_vars_in_command_and_title() {
        let mut vars = BTreeMap::new();
        vars.insert(
            crate::model::TemplateVarName::new("dir").unwrap(),
            crate::model::TemplateVarValue::new("/srv/app").unwrap(),
        );
        let node = LayoutNode::Pane {
            command: Some(PaneCommand::new("cd {{dir}}").unwrap()),
            focus: true,
            title: Some(PaneTitle::new("{{dir}}").unwrap()),
            wait_for: None,
        };
        let plan = LayoutPlan::build(&node, &vars).unwrap();
        let leaf = &plan.leaves()[0];
        assert_eq!(leaf.command.as_ref().unwrap().as_str(), "cd /srv/app");
        assert_eq!(leaf.title.as_ref().unwrap().as_str(), "/srv/app");
        assert!(leaf.focus);
    }

    #[test]
    fn wait_for_is_preserved_whole() {
        let node = LayoutNode::Pane {
            command: Some(PaneCommand::new("srv").unwrap()),
            focus: false,
            title: None,
            wait_for: Some(WaitFor {
                pattern: WaitPattern::new("ready").unwrap(),
                timeout: WaitTimeoutSeconds::new(7).unwrap(),
            }),
        };
        let plan = LayoutPlan::build(&node, &no_vars()).unwrap();
        let wf = plan.leaves()[0].wait_for.as_ref().unwrap();
        assert_eq!(wf.pattern.as_str(), "ready");
        assert_eq!(wf.timeout.as_secs(), 7);
    }

    #[test]
    fn rejects_tree_deeper_than_max_depth() {
        let too_deep = crate::test_fixtures::make_deeply_nested(crate::MAX_LAYOUT_DEPTH + 1);
        let err = LayoutPlan::build(&too_deep, &no_vars()).unwrap_err();
        assert!(err.to_string().contains("deeply nested"), "{err}");
    }

    #[test]
    fn accepts_tree_at_exactly_max_depth() {
        let ok = crate::test_fixtures::make_deeply_nested(crate::MAX_LAYOUT_DEPTH);
        assert!(LayoutPlan::build(&ok, &no_vars()).is_ok());
    }
}
