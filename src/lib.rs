//! Declarative tmux session manager.
//!
//! `nix-tmux-define` turns a declarative session description — written in JSON,
//! TOML, or YAML — into a running tmux
//! session. A [`Session`] contains ordered [`Window`]s, each holding a recursive
//! [`LayoutNode`] tree of panes and splits.
//!
//! # Two execution paths
//!
//! The same model drives two interchangeable backends:
//!
//! - [`Compiler`] — compiles a [`Session`] into a self-contained, idempotent
//!   bash script (the `print` path). Useful for dry-runs and explicit script
//!   export.
//! - [`Executor`] — drives tmux directly through the [`TmuxBackend`] trait (the
//!   `run` / `reload` path). [`RealTmux`] talks to the real binary;
//!   [`RecordingBackend`] records calls for fast, tmux-free unit and
//!   integration tests.
//!
//! Both perform the same two-phase layout traversal: first build the full pane
//! structure (`split-window`), then send commands (`send-keys`). This guarantees
//! every pane exists before any command is dispatched to it.
//!
//! # Example
//!
//! ```
//! use nix_tmux_define::{Compiler, Session};
//!
//! let session: Session = serde_json::from_str(r#"{
//!     "name": "demo",
//!     "windows": [{ "name": "main", "layout": { "type": "pane", "command": "htop" } }]
//! }"#)?;
//!
//! let mut compiler = Compiler::new();
//! compiler.compile(&session)?;
//! let script = compiler.into_script();
//! assert!(script.starts_with("#!/usr/bin/env bash"));
//! # Ok::<(), anyhow::Error>(())
//! ```

pub mod backend;
pub mod compiler;
pub mod executor;
pub mod format;
pub mod model;

pub use backend::{RealTmux, RecordingBackend, TmuxBackend};
pub use compiler::Compiler;
pub use executor::Executor;
pub use format::{load_session, load_sessions_from_dir};
pub use model::*;

/// Maximum nesting depth for layout trees, enforced in both compiler and executor.
pub(crate) const MAX_LAYOUT_DEPTH: usize = 64;

pub fn json_schema() -> anyhow::Result<String> {
    let schema = schemars::schema_for!(model::Session);
    Ok(serde_json::to_string_pretty(&schema)?)
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use crate::model::{Direction, LayoutNode, SplitRatio};

    /// Builds a right-leaning chain of Split nodes of the given depth.
    /// Used in both compiler and executor tests for depth-limit coverage.
    pub(crate) fn make_deeply_nested(depth: usize) -> LayoutNode {
        if depth == 0 {
            return LayoutNode::pane();
        }
        LayoutNode::split(
            Direction::Horizontal,
            SplitRatio::new(0.5).unwrap(),
            LayoutNode::pane(),
            make_deeply_nested(depth - 1),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_schema_is_valid_json() {
        let s = json_schema().unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).expect("schema must be valid JSON");
        assert!(
            v.get("$schema").is_some() || v.get("title").is_some() || v.get("properties").is_some(),
            "schema should contain standard JSON Schema fields"
        );
    }

    #[test]
    fn json_schema_contains_session_fields() {
        let s = json_schema().unwrap();
        assert!(
            s.contains("\"windows\""),
            "schema should reference 'windows'"
        );
        assert!(s.contains("\"name\""), "schema should reference 'name'");
    }

    #[test]
    fn json_schema_matches_split_ratio_bounds() {
        let s = json_schema().unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).expect("schema must be valid JSON");
        let ratio = v
            .pointer("/definitions/SplitRatio")
            .expect("schema should define SplitRatio");

        assert_eq!(
            ratio.get("minimum").and_then(serde_json::Value::as_f64),
            Some(SplitRatio::MIN_INCLUSIVE)
        );
        assert_eq!(
            ratio
                .get("exclusiveMaximum")
                .and_then(serde_json::Value::as_f64),
            Some(SplitRatio::MAX_EXCLUSIVE)
        );
        assert!(
            ratio.get("maximum").is_none(),
            "SplitRatio upper bound must stay exclusive"
        );
    }

    #[test]
    fn json_schema_matches_wait_timeout_bounds() {
        let s = json_schema().unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).expect("schema must be valid JSON");
        let timeout = v
            .pointer("/definitions/WaitTimeoutSeconds")
            .expect("schema should define WaitTimeoutSeconds");

        assert_eq!(
            timeout.get("type").and_then(serde_json::Value::as_str),
            Some("integer")
        );
        assert_eq!(
            timeout.get("minimum").and_then(serde_json::Value::as_f64),
            Some(f64::from(WaitTimeoutSeconds::MIN_INCLUSIVE))
        );
        assert!(
            timeout.get("exclusiveMinimum").is_none(),
            "WaitTimeoutSeconds lower bound must stay inclusive"
        );
    }
}
