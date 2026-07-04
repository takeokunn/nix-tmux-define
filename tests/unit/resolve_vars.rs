//! Property-based and example tests for `resolve_vars` template substitution.

use std::collections::BTreeMap;

use nix_tmux_define::{resolve_vars, TemplateVarName, TemplateVarValue};
use proptest::prelude::*;

fn var_key(value: &str) -> TemplateVarName {
    TemplateVarName::new(value).unwrap()
}

fn var_value(value: &str) -> TemplateVarValue {
    TemplateVarValue::new(value).unwrap()
}

proptest! {
    /// A string containing no `{{` placeholder is returned unchanged, no matter
    /// what user variables are defined.
    #[test]
    fn identity_without_placeholder(s in "[^{]{0,40}") {
        let mut vars = BTreeMap::new();
        vars.insert(var_key("unused"), var_value("X"));
        prop_assert_eq!(resolve_vars(&s, &vars), s);
    }

    /// A user variable is substituted wherever its `{{key}}` placeholder occurs,
    /// for any value that is not itself a placeholder.
    #[test]
    fn substitutes_user_var(value in "[a-zA-Z0-9 _/.-]{0,24}") {
        let mut vars = BTreeMap::new();
        vars.insert(var_key("k"), var_value(&value));
        prop_assert_eq!(resolve_vars("a {{k}} b", &vars), format!("a {value} b"));
    }
}

#[test]
fn expands_builtins() {
    let vars = BTreeMap::new();
    assert_eq!(resolve_vars("{{cwd}}", &vars), "$PWD");
    assert_eq!(resolve_vars("{{date}}", &vars), "$(date +%Y-%m-%d)");
    assert_eq!(
        resolve_vars("{{git_branch}}", &vars),
        "$(git rev-parse --abbrev-ref HEAD)"
    );
}

#[test]
fn unknown_placeholder_passes_through() {
    // Intentional: `{{ }}` appears in real commands such as `awk '{{print}}'`,
    // so unknown placeholders must NOT be treated as errors or stripped.
    let vars = BTreeMap::new();
    assert_eq!(resolve_vars("awk '{{print}}'", &vars), "awk '{{print}}'");
    assert_eq!(resolve_vars("{{unknown}}", &vars), "{{unknown}}");
}

#[test]
fn user_var_overrides_builtin_name() {
    // User vars are applied before built-ins, so a user var named like a
    // built-in wins.
    let mut vars = BTreeMap::new();
    vars.insert(var_key("cwd"), var_value("/custom"));
    assert_eq!(resolve_vars("{{cwd}}", &vars), "/custom");
}
