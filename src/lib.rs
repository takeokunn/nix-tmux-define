pub mod backend;
pub mod compiler;
pub mod executor;
pub mod format;
pub mod model;

pub use backend::{RecordingBackend, RealTmux, TmuxBackend};
pub use compiler::Compiler;
pub use executor::Executor;
pub use format::{load_session, load_sessions_from_dir};
pub use model::*;

pub fn json_schema() -> String {
    let schema = schemars::schema_for!(model::Session);
    serde_json::to_string_pretty(&schema).expect("schema serialization")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_schema_is_valid_json() {
        let s = json_schema();
        let v: serde_json::Value = serde_json::from_str(&s).expect("schema must be valid JSON");
        assert!(v.get("$schema").is_some() || v.get("title").is_some() || v.get("properties").is_some(),
            "schema should contain standard JSON Schema fields");
    }

    #[test]
    fn json_schema_contains_session_fields() {
        let s = json_schema();
        assert!(s.contains("\"windows\""), "schema should reference 'windows'");
        assert!(s.contains("\"name\""), "schema should reference 'name'");
    }
}
