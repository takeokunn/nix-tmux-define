//! Integration-tier crate: multiple components wired together against mock
//! backends. These tests must not shell out to the real `tmux` binary.

mod tmux;
