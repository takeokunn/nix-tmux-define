//! Unit-tier integration crate: property-based tests of the pure, public,
//! security-critical helpers (`shell_quote`, `resolve_vars`).
//!
//! Example-based unit tests that exercise internals live in `src/` alongside the
//! code; this tier holds the property-based tests, split by subject.

mod resolve_vars;
mod shell_quote;
