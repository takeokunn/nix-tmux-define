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

pub fn json_schema() -> String {
    let schema = schemars::schema_for!(model::Session);
    serde_json::to_string_pretty(&schema).expect("schema serialization")
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use crate::model::{Direction, LayoutNode};

    /// Builds a right-leaning chain of Split nodes of the given depth.
    /// Used in both compiler and executor tests for depth-limit coverage.
    pub(crate) fn make_deeply_nested(depth: usize) -> LayoutNode {
        if depth == 0 {
            return LayoutNode::Pane {
                command: None,
                focus: false,
                title: None,
                wait_for: None,
            };
        }
        LayoutNode::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(LayoutNode::Pane {
                command: None,
                focus: false,
                title: None,
                wait_for: None,
            }),
            second: Box::new(make_deeply_nested(depth - 1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_schema_is_valid_json() {
        let s = json_schema();
        let v: serde_json::Value = serde_json::from_str(&s).expect("schema must be valid JSON");
        assert!(
            v.get("$schema").is_some() || v.get("title").is_some() || v.get("properties").is_some(),
            "schema should contain standard JSON Schema fields"
        );
    }

    #[test]
    fn json_schema_contains_session_fields() {
        let s = json_schema();
        assert!(
            s.contains("\"windows\""),
            "schema should reference 'windows'"
        );
        assert!(s.contains("\"name\""), "schema should reference 'name'");
    }
}
