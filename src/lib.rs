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
