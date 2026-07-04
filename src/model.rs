use anyhow::Context;
use schemars::{
    r#gen::SchemaGenerator,
    schema::{InstanceType, NumberValidation, Schema, SchemaObject, StringValidation},
    JsonSchema,
};
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::ops::Deref;
use std::process::Command;

// ─── Validators ──────────────────────────────────────────────────────────────

fn deserialize_windows<'de, D>(deserializer: D) -> Result<Vec<Window>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let windows = Vec::<Window>::deserialize(deserializer)?;
    if windows.is_empty() {
        return Err(D::Error::custom("session must define at least one window"));
    }
    Ok(windows)
}

// ─── Model ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct Session {
    pub name: TmuxName,
    /// Default working directory for all panes; falls back to `$HOME`
    pub root: Option<RootTemplate>,
    /// Ordered list of windows; a session must contain at least one
    #[serde(deserialize_with = "deserialize_windows")]
    pub windows: Vec<Window>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Shell command executed before the session is created (e.g. `nix build`)
    pub pre_hook: Option<ShellCommand>,
    /// tmux set-option key/value pairs for the session
    #[serde(default)]
    pub options: TmuxOptions,
    /// Template variables substituted via {{key}} in commands/roots
    #[serde(default)]
    pub vars: TemplateVars,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct Window {
    pub name: TmuxName,
    pub layout: LayoutNode,
    /// Working directory for this window; overrides the session root when set
    pub root: Option<RootTemplate>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// tmux set-window-option key/value pairs for this window
    #[serde(default)]
    pub options: TmuxOptions,
    /// Layout preset (e.g. "tiled", "even-horizontal", "even-vertical")
    #[serde(default)]
    pub select_layout: Option<TmuxLayoutPreset>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct EnvVar {
    pub key: EnvVarName,
    pub value: EnvVarValue,
}

/// Fraction of space allocated to the first split child.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SplitRatio(f64);

impl SplitRatio {
    pub const MIN_INCLUSIVE: f64 = 0.006;
    pub const MAX_EXCLUSIVE: f64 = 0.99;

    pub fn new(value: f64) -> anyhow::Result<Self> {
        if !value.is_finite() || !(Self::MIN_INCLUSIVE..Self::MAX_EXCLUSIVE).contains(&value) {
            anyhow::bail!(
                "split ratio must be between {} (inclusive) and {} (exclusive), got {value}",
                Self::MIN_INCLUSIVE,
                Self::MAX_EXCLUSIVE
            );
        }
        Ok(Self(value))
    }

    pub fn get(self) -> f64 {
        self.0
    }

    pub fn tmux_second_pane_percent(self) -> TmuxPanePercent {
        let percent = ((1.0 - self.0) * 100.0).round() as u32;
        TmuxPanePercent::from_split_ratio_percent(percent)
    }
}

/// Pane size accepted by `tmux split-window -l`, expressed as a percentage.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TmuxPanePercent(u8);

impl TmuxPanePercent {
    pub const MIN: u32 = 1;
    pub const MAX: u32 = 99;

    pub fn new(value: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (Self::MIN..=Self::MAX).contains(&value),
            "tmux pane percent must be between {} and {}, got {value}",
            Self::MIN,
            Self::MAX
        );
        Ok(Self(value as u8))
    }

    fn from_split_ratio_percent(value: u32) -> Self {
        debug_assert!((Self::MIN..=Self::MAX).contains(&value));
        Self(value as u8)
    }

    pub fn as_u32(self) -> u32 {
        u32::from(self.0)
    }

    pub fn as_tmux_size(self) -> String {
        format!("{}%", self.as_u32())
    }
}

impl TryFrom<f64> for SplitRatio {
    type Error = anyhow::Error;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for SplitRatio {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl JsonSchema for SplitRatio {
    fn schema_name() -> String {
        "SplitRatio".to_owned()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::Number.into()),
            format: Some("double".to_owned()),
            number: Some(Box::new(NumberValidation {
                minimum: Some(Self::MIN_INCLUSIVE),
                exclusive_maximum: Some(Self::MAX_EXCLUSIVE),
                ..NumberValidation::default()
            })),
            ..SchemaObject::default()
        })
    }
}

/// A node in the recursive pane-layout tree.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LayoutNode {
    Pane {
        /// Command sent to this pane on startup via `send-keys`
        #[serde(default)]
        command: Option<PaneCommand>,
        /// Moves focus to this pane after the session is fully built
        #[serde(default)]
        focus: bool,
        /// Sets the pane title via `select-pane -T`
        #[serde(default)]
        title: Option<PaneTitle>,
        /// Wait for a pattern in pane output before continuing
        #[serde(default)]
        wait_for: Option<WaitFor>,
    },
    Split {
        direction: Direction,
        /// Fraction of space allocated to the *first* child.
        ratio: SplitRatio,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct WaitFor {
    pub pattern: WaitPattern,
    #[serde(default = "default_timeout")]
    pub timeout: WaitTimeoutSeconds,
}

/// Non-empty pane-output pattern used by `wait_for`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct WaitPattern(String);

impl WaitPattern {
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        validate_wait_pattern(&value, "wait_for pattern")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for WaitPattern {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for WaitPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(f)
    }
}

impl<'de> Deserialize<'de> for WaitPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl JsonSchema for WaitPattern {
    fn schema_name() -> String {
        "WaitPattern".to_owned()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                min_length: Some(1),
                pattern: Some(r"^[^\u0000]+$".to_owned()),
                ..StringValidation::default()
            })),
            ..SchemaObject::default()
        })
    }
}

/// Positive timeout in seconds for `wait_for` polling.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WaitTimeoutSeconds(u32);

impl WaitTimeoutSeconds {
    pub const MIN_INCLUSIVE: u32 = 1;
    pub const DEFAULT_SECONDS: u32 = 30;
    pub const DEFAULT: Self = Self(Self::DEFAULT_SECONDS);

    pub fn new(value: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(
            value >= Self::MIN_INCLUSIVE,
            "wait_for timeout must be at least {} second",
            Self::MIN_INCLUSIVE
        );
        Ok(Self(value))
    }

    pub fn as_secs(self) -> u32 {
        self.0
    }
}

impl Default for WaitTimeoutSeconds {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl From<WaitTimeoutSeconds> for u32 {
    fn from(value: WaitTimeoutSeconds) -> Self {
        value.as_secs()
    }
}

impl<'de> Deserialize<'de> for WaitTimeoutSeconds {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl JsonSchema for WaitTimeoutSeconds {
    fn schema_name() -> String {
        "WaitTimeoutSeconds".to_owned()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::Integer.into()),
            format: Some("uint32".to_owned()),
            number: Some(Box::new(NumberValidation {
                minimum: Some(f64::from(Self::MIN_INCLUSIVE)),
                ..NumberValidation::default()
            })),
            ..SchemaObject::default()
        })
    }
}

fn default_timeout() -> WaitTimeoutSeconds {
    WaitTimeoutSeconds::DEFAULT
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Horizontal,
    Vertical,
}

impl Direction {
    pub fn tmux_split_flag(self) -> TmuxSplitFlag {
        match self {
            Direction::Horizontal => TmuxSplitFlag::Horizontal,
            Direction::Vertical => TmuxSplitFlag::Vertical,
        }
    }
}

/// Only the tmux split flags supported by declarative layouts.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum TmuxSplitFlag {
    Horizontal,
    Vertical,
}

impl TmuxSplitFlag {
    pub fn as_str(self) -> &'static str {
        match self {
            TmuxSplitFlag::Horizontal => "-h",
            TmuxSplitFlag::Vertical => "-v",
        }
    }
}

// ─── Convenience constructors ──────────────────────────────────────────────────

impl LayoutNode {
    /// An empty leaf pane: no command, not focused, no title, no `wait_for`.
    ///
    /// ```
    /// use nix_tmux_define::LayoutNode;
    /// assert_eq!(LayoutNode::pane(), LayoutNode::default());
    /// ```
    pub fn pane() -> Self {
        LayoutNode::Pane {
            command: None,
            focus: false,
            title: None,
            wait_for: None,
        }
    }

    /// A leaf pane that runs `command` on startup.
    ///
    /// ```
    /// use nix_tmux_define::LayoutNode;
    /// let node = LayoutNode::command("htop").unwrap();
    /// assert!(matches!(node, LayoutNode::Pane { command: Some(c), .. } if c.as_str() == "htop"));
    /// ```
    pub fn command(command: impl Into<String>) -> anyhow::Result<Self> {
        Ok(LayoutNode::Pane {
            command: Some(PaneCommand::new(command)?),
            focus: false,
            title: None,
            wait_for: None,
        })
    }

    /// A split of two children.
    ///
    /// ```
    /// use nix_tmux_define::{Direction, LayoutNode, SplitRatio};
    /// let node = LayoutNode::split(
    ///     Direction::Horizontal,
    ///     SplitRatio::new(0.6).unwrap(),
    ///     LayoutNode::command("nvim .").unwrap(),
    ///     LayoutNode::pane(),
    /// );
    /// assert!(matches!(node, LayoutNode::Split { .. }));
    /// ```
    pub fn split(
        direction: Direction,
        ratio: SplitRatio,
        first: LayoutNode,
        second: LayoutNode,
    ) -> Self {
        LayoutNode::Split {
            direction,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    pub fn try_split(
        direction: Direction,
        ratio: f64,
        first: LayoutNode,
        second: LayoutNode,
    ) -> anyhow::Result<Self> {
        Ok(Self::split(
            direction,
            SplitRatio::new(ratio)?,
            first,
            second,
        ))
    }
}

impl Default for LayoutNode {
    fn default() -> Self {
        LayoutNode::pane()
    }
}

// ─── Shell Quoting ────────────────────────────────────────────────────────────

// ─── Runtime validation ───────────────────────────────────────────────────────

/// Validates a tmux name using the same rules as `deserialize_tmux_name`.
///
/// Called at the start of `Compiler::compile` and `Executor::run`/`reload` so
/// that `Session` structs constructed directly (bypassing serde) are also safe.
pub fn validate_tmux_name(s: &str, context: &str) -> anyhow::Result<()> {
    if s.is_empty() {
        anyhow::bail!("{context}: tmux name must not be empty");
    }
    if let Some(ch) = s.chars().find(|&c| c == ':' || c == '.') {
        anyhow::bail!("{context}: tmux name must not contain {ch:?} (tmux target separator)");
    }
    if let Some(ch) = s.chars().find(|c| c.is_control()) {
        anyhow::bail!("{context}: tmux name must not contain control characters (found {ch:?})");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TmuxName(String);

impl TmuxName {
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        validate_tmux_name(&value, "tmux name")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for TmuxName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Deref for TmuxName {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl Borrow<str> for TmuxName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<&str> for TmuxName {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<TmuxName> for &str {
    fn eq(&self, other: &TmuxName) -> bool {
        *self == other.as_str()
    }
}

impl std::fmt::Display for TmuxName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(f)
    }
}

impl<'de> Deserialize<'de> for TmuxName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl JsonSchema for TmuxName {
    fn schema_name() -> String {
        "TmuxName".to_owned()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                pattern: Some(r"^[^:.\u0000-\u001F\u007F]+$".to_owned()),
                ..StringValidation::default()
            })),
            ..SchemaObject::default()
        })
    }
}

fn validate_env_key(s: &str, context: &str) -> anyhow::Result<()> {
    if s.is_empty() {
        anyhow::bail!("{context}: env var key must not be empty");
    }
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        anyhow::bail!("{context}: env var key must not be empty");
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        anyhow::bail!(
            "{context}: invalid env var key {s:?}: must start with ASCII letter or underscore"
        );
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        anyhow::bail!(
            "{context}: invalid env var key {s:?}: must contain only ASCII letters, digits, or underscores"
        );
    }
    Ok(())
}

fn validate_no_nul(s: &str, context: &str) -> anyhow::Result<()> {
    if s.contains('\0') {
        anyhow::bail!("{context}: must not contain NUL bytes");
    }
    Ok(())
}

fn validate_string_no_nul(s: &str, context: &str) -> anyhow::Result<()> {
    validate_no_nul(s, context)
}

fn validate_non_empty_no_nul(s: &str, context: &str) -> anyhow::Result<()> {
    validate_no_nul(s, context)?;
    anyhow::ensure!(!s.is_empty(), "{context}: must not be empty");
    Ok(())
}

fn validate_template_var_name(s: &str, context: &str) -> anyhow::Result<()> {
    validate_env_key(s, context)
}

fn validate_tmux_option_name(s: &str, context: &str) -> anyhow::Result<()> {
    validate_no_nul(s, context)?;
    if s.is_empty() {
        anyhow::bail!("{context}: tmux option key must not be empty");
    }
    if s.starts_with('-') {
        anyhow::bail!(
            "{context}: invalid tmux option key {s:?}: must not start with '-' (would be parsed as a flag)"
        );
    }
    Ok(())
}

fn validate_tmux_option_value(s: &str, context: &str) -> anyhow::Result<()> {
    validate_no_nul(s, context)
}

fn validate_tmux_layout_preset(s: &str, context: &str) -> anyhow::Result<()> {
    validate_no_nul(s, context)?;
    anyhow::ensure!(
        !s.is_empty(),
        "{context}: tmux layout preset must not be empty"
    );
    Ok(())
}

fn validate_wait_pattern(s: &str, context: &str) -> anyhow::Result<()> {
    if s.is_empty() {
        anyhow::bail!("{context}: wait_for pattern must not be empty");
    }
    validate_no_nul(s, context)
}

macro_rules! define_validated_string_type {
    ($type_name:ident, $validator:ident, $context:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $type_name(String);

        impl $type_name {
            pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
                let value = value.into();
                $validator(&value, $context)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $type_name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl Deref for $type_name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                self.as_str()
            }
        }

        impl Borrow<str> for $type_name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }

        impl PartialEq<&str> for $type_name {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }

        impl PartialEq<$type_name> for &str {
            fn eq(&self, other: &$type_name) -> bool {
                *self == other.as_str()
            }
        }

        impl std::fmt::Display for $type_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(self.as_str(), f)
            }
        }

        impl<'de> Deserialize<'de> for $type_name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                use serde::de::Error;
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }

        impl JsonSchema for $type_name {
            fn schema_name() -> String {
                stringify!($type_name).to_owned()
            }

            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                Schema::Object(SchemaObject {
                    instance_type: Some(InstanceType::String.into()),
                    string: Some(Box::new(StringValidation {
                        pattern: Some(r"^[^\u0000]*$".to_owned()),
                        ..StringValidation::default()
                    })),
                    ..SchemaObject::default()
                })
            }
        }
    };
}

define_validated_string_type!(PaneCommand, validate_string_no_nul, "pane command");
define_validated_string_type!(PaneTitle, validate_string_no_nul, "pane title");
define_validated_string_type!(ShellCommand, validate_string_no_nul, "shell command");
define_validated_string_type!(RootTemplate, validate_non_empty_no_nul, "root template");
define_validated_string_type!(EnvVarName, validate_env_key, "env var key");
define_validated_string_type!(EnvVarValue, validate_string_no_nul, "env var value");
define_validated_string_type!(TmuxOptionName, validate_tmux_option_name, "tmux option key");
define_validated_string_type!(
    TmuxOptionValue,
    validate_tmux_option_value,
    "tmux option value"
);
define_validated_string_type!(
    TemplateVarName,
    validate_template_var_name,
    "template var name"
);
define_validated_string_type!(
    TemplateVarValue,
    validate_string_no_nul,
    "template var value"
);
define_validated_string_type!(
    TmuxLayoutPreset,
    validate_tmux_layout_preset,
    "tmux layout preset"
);

pub type TmuxOptions = BTreeMap<TmuxOptionName, TmuxOptionValue>;
pub type TemplateVars = BTreeMap<TemplateVarName, TemplateVarValue>;

fn validate_env_var(ev: &EnvVar, context: &str) -> anyhow::Result<()> {
    validate_env_key(ev.key.as_str(), &format!("{context} key"))?;
    validate_no_nul(ev.value.as_str(), &format!("{context} value"))
}

fn validate_tmux_options(options: &TmuxOptions, context: &str) -> anyhow::Result<()> {
    for (key, value) in options {
        validate_tmux_option_name(key.as_str(), &format!("{context} key {key:?}"))?;
        validate_tmux_option_value(value.as_str(), &format!("{context} value for {key:?}"))?;
    }
    Ok(())
}

fn validate_wait_for(wait_for: &WaitFor, context: &str) -> anyhow::Result<()> {
    validate_wait_pattern(wait_for.pattern.as_str(), &format!("{context} pattern"))
}

fn validate_root(root: &Option<RootTemplate>, context: &str) -> anyhow::Result<()> {
    let Some(root) = root else {
        return Ok(());
    };
    validate_non_empty_no_nul(root.as_str(), context)
}

fn validate_layout(node: &LayoutNode, context: &str, depth: usize) -> anyhow::Result<()> {
    if depth > crate::MAX_LAYOUT_DEPTH {
        anyhow::bail!(
            "{context}: layout tree is too deeply nested (max depth: {})",
            crate::MAX_LAYOUT_DEPTH
        );
    }
    match node {
        LayoutNode::Pane {
            command,
            title,
            wait_for,
            ..
        } => {
            if let Some(command) = command {
                PaneCommand::new(command.as_str().to_owned())
                    .with_context(|| format!("{context} command"))?;
            }
            if let Some(title) = title {
                PaneTitle::new(title.as_str().to_owned())
                    .with_context(|| format!("{context} title"))?;
            }
            if let Some(wait_for) = wait_for {
                validate_wait_for(wait_for, &format!("{context} wait_for"))?;
            }
        }
        LayoutNode::Split { first, second, .. } => {
            validate_layout(first, &format!("{context}.first"), depth + 1)?;
            validate_layout(second, &format!("{context}.second"), depth + 1)?;
        }
    }
    Ok(())
}

impl Session {
    /// Re-runs the critical serde validators so callers who construct `Session`
    /// directly (without going through `load_session`) get the same guarantees.
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_tmux_name(self.name.as_str(), "session name")?;
        validate_root(&self.root, "session root")?;
        for ev in &self.env {
            validate_env_var(ev, &format!("session env {:?}", ev.key))?;
        }
        if let Some(pre_hook) = &self.pre_hook {
            ShellCommand::new(pre_hook.as_str().to_owned()).context("session pre_hook")?;
        }
        validate_tmux_options(&self.options, "session options")?;
        for (key, value) in &self.vars {
            validate_template_var_name(key.as_str(), &format!("vars key {key:?}"))?;
            validate_no_nul(value.as_str(), &format!("vars value for {key:?}"))?;
        }
        if self.windows.is_empty() {
            anyhow::bail!("session must define at least one window");
        }
        let mut window_names = HashSet::new();
        for w in &self.windows {
            validate_tmux_name(w.name.as_str(), &format!("window name {:?}", w.name))?;
            if !window_names.insert(w.name.as_str()) {
                anyhow::bail!("duplicate window name {:?}", w.name);
            }
            validate_root(&w.root, &format!("window {:?} root", w.name))?;
            for ev in &w.env {
                validate_env_var(ev, &format!("window {:?} env {:?}", w.name, ev.key))?;
            }
            validate_tmux_options(&w.options, &format!("window {:?} options", w.name))?;
            if let Some(select_layout) = &w.select_layout {
                TmuxLayoutPreset::new(select_layout.as_str().to_owned())
                    .with_context(|| format!("window {:?} select_layout", w.name))?;
            }
            validate_layout(&w.layout, &format!("window {:?}", w.name), 0)?;
        }
        Ok(())
    }
}

// ─── Shell Quoting ────────────────────────────────────────────────────────────

/// POSIX single-quote escape: wraps `s` in `'…'`, turning interior `'` into `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTmuxArg(String);

impl ResolvedTmuxArg {
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        validate_no_nul(&value, "resolved tmux argument")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for ResolvedTmuxArg {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellWord(String);

impl ShellWord {
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        validate_no_nul(&value, "shell word")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for ShellWord {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneId(String);

impl PaneId {
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        validate_no_nul(&value, "pane id")?;
        anyhow::ensure!(!value.is_empty(), "pane id must not be empty");

        let mut chars = value.chars();
        let has_tmux_prefix = chars.next() == Some('%');
        let has_only_digits = chars.all(|c| c.is_ascii_digit());
        anyhow::ensure!(
            has_tmux_prefix && has_only_digits && value.len() > 1,
            "pane id must be a tmux pane id like %0, got {value:?}"
        );

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for PaneId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

// ─── Template Variable Resolution ────────────────────────────────────────────

/// Substitutes `{{key}}` from the `vars` map, then built-in dynamic vars:
/// - `{{cwd}}` → `$PWD`
/// - `{{date}}` → `$(date +%Y-%m-%d)`
/// - `{{git_branch}}` → `$(git rev-parse --abbrev-ref HEAD)`
fn resolve_builtin_var(key: &str) -> Option<&'static str> {
    match key {
        "cwd" => Some("$PWD"),
        "date" => Some("$(date +%Y-%m-%d)"),
        "git_branch" => Some("$(git rev-parse --abbrev-ref HEAD)"),
        _ => None,
    }
}

pub fn resolve_vars(s: &str, vars: &TemplateVars) -> String {
    // Single left-to-right scan: each {{key}} is replaced with its value from
    // vars exactly once. Replacement values are NOT re-scanned, so a var value
    // that itself contains {{other}} is never double-expanded regardless of key
    // alphabetical order.
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(start) = rest.find("{{") {
            result.push_str(&rest[..start]);
            let after_open = &rest[start + 2..];
            if let Some(end) = after_open.find("}}") {
                let key = &after_open[..end];
                if let Some(val) = vars.get(key) {
                    result.push_str(val.as_str());
                } else if let Some(val) = resolve_builtin_var(key) {
                    result.push_str(val);
                } else {
                    // Unknown key: preserve the placeholder literally.
                    result.push_str("{{");
                    result.push_str(key);
                    result.push_str("}}");
                }
                rest = &after_open[end + 2..];
            } else {
                // Unclosed "{{": output literally and continue
                result.push_str("{{");
                rest = after_open;
            }
        } else {
            result.push_str(rest);
            break;
        }
    }
    result
}

fn run_builtin_command(key: &str, program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to resolve template variable {key:?} using {program}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            anyhow::bail!(
                "failed to resolve template variable {key:?}: {program} exited with {}",
                output.status
            );
        }
        anyhow::bail!("failed to resolve template variable {key:?}: {stderr}");
    }

    decode_builtin_command_stdout(key, program, &output.stdout)
}

fn decode_builtin_command_stdout(
    key: &str,
    program: &str,
    stdout: &[u8],
) -> anyhow::Result<String> {
    Ok(std::str::from_utf8(stdout)
        .with_context(|| {
            format!(
                "failed to resolve template variable {key:?}: {program} returned non-UTF-8 output"
            )
        })?
        .trim()
        .to_owned())
}

fn os_string_to_tmux_arg(value: OsString, context: &str) -> anyhow::Result<String> {
    value.into_string().map_err(|_| {
        anyhow::anyhow!(
            "{context} contains non-UTF-8 bytes and cannot be passed as a tmux argument"
        )
    })
}

fn resolve_builtin_tmux_arg_var(key: &str) -> anyhow::Result<Option<String>> {
    match key {
        "cwd" => Ok(Some(os_string_to_tmux_arg(
            std::env::current_dir()
                .context("failed to resolve current directory for {{cwd}}")?
                .into_os_string(),
            "current directory for {{cwd}}",
        )?)),
        "date" => Ok(Some(run_builtin_command("date", "date", &["+%Y-%m-%d"])?)),
        "git_branch" => Ok(Some(run_builtin_command(
            "git_branch",
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"],
        )?)),
        _ => Ok(None),
    }
}

pub fn resolve_tmux_arg_vars(s: &str, vars: &TemplateVars) -> anyhow::Result<ResolvedTmuxArg> {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(start) = rest.find("{{") {
            result.push_str(&rest[..start]);
            let after_open = &rest[start + 2..];
            if let Some(end) = after_open.find("}}") {
                let key = &after_open[..end];
                if let Some(val) = vars.get(key) {
                    result.push_str(val.as_str());
                } else if let Some(val) = resolve_builtin_tmux_arg_var(key)? {
                    result.push_str(&val);
                } else {
                    result.push_str("{{");
                    result.push_str(key);
                    result.push_str("}}");
                }
                rest = &after_open[end + 2..];
            } else {
                result.push_str("{{");
                rest = after_open;
            }
        } else {
            result.push_str(rest);
            break;
        }
    }
    ResolvedTmuxArg::new(result)
}

fn resolve_builtin_shell_arg_var(key: &str) -> Option<&'static str> {
    match key {
        "cwd" => Some("\"${PWD}\""),
        "date" => Some("\"$(date +%Y-%m-%d)\""),
        "git_branch" => Some("\"$(git rev-parse --abbrev-ref HEAD)\""),
        _ => None,
    }
}

fn push_shell_literal(out: &mut String, literal: &str) {
    if !literal.is_empty() {
        out.push_str(&shell_quote(literal));
    }
}

pub fn shell_quote_template_vars(s: &str, vars: &TemplateVars) -> anyhow::Result<ShellWord> {
    let mut result = String::new();
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(start) = rest.find("{{") {
            push_shell_literal(&mut result, &rest[..start]);
            let after_open = &rest[start + 2..];
            if let Some(end) = after_open.find("}}") {
                let key = &after_open[..end];
                if let Some(val) = vars.get(key) {
                    push_shell_literal(&mut result, val.as_str());
                } else if let Some(val) = resolve_builtin_shell_arg_var(key) {
                    result.push_str(val);
                } else {
                    push_shell_literal(&mut result, &format!("{{{{{key}}}}}"));
                }
                rest = &after_open[end + 2..];
            } else {
                push_shell_literal(&mut result, "{{");
                rest = after_open;
            }
        } else {
            push_shell_literal(&mut result, rest);
            break;
        }
    }

    let word = if result.is_empty() {
        shell_quote("")
    } else {
        result
    };
    ShellWord::new(word)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod validate_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn tmux_name(value: &str) -> TmuxName {
        TmuxName::new(value).unwrap()
    }

    fn root_template(value: &str) -> RootTemplate {
        RootTemplate::new(value).unwrap()
    }

    fn valid_session(name: &str) -> Session {
        Session {
            name: tmux_name(name),
            root: None,
            windows: vec![Window {
                name: tmux_name("w"),
                layout: LayoutNode::pane(),
                root: None,
                env: vec![],
                options: BTreeMap::new(),
                select_layout: None,
            }],
            env: vec![],
            pre_hook: None,
            options: BTreeMap::new(),
            vars: BTreeMap::new(),
        }
    }

    #[test]
    fn validate_accepts_valid_session() {
        assert!(valid_session("my-session").validate().is_ok());
    }

    #[test]
    fn validate_rejects_colon_in_name() {
        let err = TmuxName::new("bad:name").unwrap_err();
        assert!(err.to_string().contains("':'"), "{err}");
    }

    #[test]
    fn validate_rejects_empty_windows() {
        let mut s = valid_session("s");
        s.windows.clear();
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_colon_in_window_name() {
        let err = TmuxName::new("bad:win").unwrap_err();
        assert!(err.to_string().contains("':'"), "{err}");
    }

    #[test]
    fn validate_rejects_direct_invalid_env_key() {
        let err = EnvVarName::new("1BAD").unwrap_err();
        assert!(err.to_string().contains("must start with"), "{err}");
    }

    #[test]
    fn validate_rejects_direct_env_nul_value() {
        let err = EnvVarValue::new("bad\0value").unwrap_err();
        assert!(err.to_string().contains("NUL"), "{err}");
    }

    #[test]
    fn validate_rejects_direct_tmux_option_flag_key() {
        let err = TmuxOptionName::new("-g").unwrap_err();
        assert!(err.to_string().contains("must not start"), "{err}");
    }

    #[test]
    fn validate_rejects_direct_empty_select_layout() {
        let err = TmuxLayoutPreset::new("").unwrap_err();
        assert!(format!("{err:#}").contains("layout preset"), "{err:#}");
    }

    #[test]
    fn validate_rejects_duplicate_window_names() {
        let mut s = valid_session("s");
        s.windows.push(s.windows[0].clone());
        let err = s.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate window"), "{err}");
    }

    #[test]
    fn validate_layout_depth_accepts_execution_limit() {
        let mut s = valid_session("s");
        s.windows[0].layout = crate::test_fixtures::make_deeply_nested(crate::MAX_LAYOUT_DEPTH);

        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_layout_depth_rejects_deeper_than_execution_limit() {
        let mut s = valid_session("s");
        s.windows[0].layout = crate::test_fixtures::make_deeply_nested(crate::MAX_LAYOUT_DEPTH + 1);

        let err = s.validate().unwrap_err();

        assert!(err.to_string().contains("deeply nested"), "{err}");
    }

    #[test]
    fn validate_accepts_dynamic_builtin_root() {
        let mut s = valid_session("s");
        s.root = Some(root_template("{{cwd}}/project"));
        assert!(s.validate().is_ok());
    }

    #[test]
    fn wait_pattern_rejects_direct_empty_pattern() {
        let err = WaitPattern::new("").unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "{err}");
    }

    #[test]
    fn wait_timeout_seconds_rejects_zero() {
        let err = WaitTimeoutSeconds::new(0).unwrap_err();
        assert!(err.to_string().contains("at least 1"), "{err}");
    }

    #[test]
    fn split_ratio_rejects_direct_invalid_ratio() {
        let err = SplitRatio::new(0.995).unwrap_err();
        assert!(err.to_string().contains("split ratio"), "{err}");
    }

    #[test]
    fn try_split_rejects_invalid_ratio() {
        let err = LayoutNode::try_split(
            Direction::Horizontal,
            0.995,
            LayoutNode::pane(),
            LayoutNode::pane(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("split ratio"), "{err}");
    }

    #[test]
    fn split_ratio_converts_to_tmux_second_pane_percent() {
        assert_eq!(
            SplitRatio::new(0.6)
                .unwrap()
                .tmux_second_pane_percent()
                .as_u32(),
            40
        );
        assert_eq!(
            SplitRatio::new(0.006)
                .unwrap()
                .tmux_second_pane_percent()
                .as_u32(),
            99
        );
        assert_eq!(
            SplitRatio::new(0.98)
                .unwrap()
                .tmux_second_pane_percent()
                .as_u32(),
            2
        );
    }

    #[test]
    fn tmux_pane_percent_rejects_out_of_range() {
        let too_small = TmuxPanePercent::new(0).unwrap_err();
        assert!(
            too_small.to_string().contains("tmux pane percent"),
            "{too_small}"
        );

        let too_large = TmuxPanePercent::new(100).unwrap_err();
        assert!(
            too_large.to_string().contains("tmux pane percent"),
            "{too_large}"
        );
    }

    #[test]
    fn direction_maps_to_typed_tmux_split_flags() {
        assert_eq!(
            Direction::Horizontal.tmux_split_flag(),
            TmuxSplitFlag::Horizontal
        );
        assert_eq!(
            Direction::Vertical.tmux_split_flag(),
            TmuxSplitFlag::Vertical
        );
        assert_eq!(TmuxSplitFlag::Horizontal.as_str(), "-h");
        assert_eq!(TmuxSplitFlag::Vertical.as_str(), "-v");
    }

    #[test]
    fn validate_rejects_direct_nul_in_command_and_pre_hook() {
        let err = LayoutNode::command("echo bad\0").unwrap_err();
        assert!(format!("{err:#}").contains("NUL"), "{err:#}");

        let err = ShellCommand::new("echo bad\0").unwrap_err();
        assert!(format!("{err:#}").contains("NUL"), "{err:#}");
    }

    #[test]
    fn validated_payload_newtypes_reject_invalid_inputs() {
        assert!(PaneCommand::new("bad\0command").is_err());
        assert!(PaneTitle::new("bad\0title").is_err());
        assert!(ShellCommand::new("bad\0hook").is_err());
        assert!(TmuxOptionName::new("").is_err());
        assert!(TmuxOptionName::new("-g").is_err());
        assert!(TmuxOptionValue::new("bad\0value").is_err());
        assert!(TmuxLayoutPreset::new("").is_err());
        assert!(TmuxLayoutPreset::new("bad\0layout").is_err());
    }

    #[test]
    fn serde_rejects_invalid_session_name() {
        let err = serde_json::from_str::<Session>(
            r#"{"name":"bad:name","windows":[{"name":"w","layout":{"type":"pane"}}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("':'"), "{err}");
    }

    #[test]
    fn serde_rejects_invalid_env_payloads() {
        let err = serde_json::from_str::<Session>(
            r#"{"name":"s","windows":[{"name":"w","layout":{"type":"pane"}}],"env":[{"key":"1BAD","value":"v"}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("must start with"), "{err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var_key(value: &str) -> TemplateVarName {
        TemplateVarName::new(value).unwrap()
    }

    fn var_value(value: &str) -> TemplateVarValue {
        TemplateVarValue::new(value).unwrap()
    }

    #[test]
    fn shell_quote_plain() {
        assert_eq!(shell_quote("hello"), "'hello'");
    }

    #[test]
    fn shell_quote_with_spaces() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }

    #[test]
    fn shell_quote_with_apostrophe() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn resolve_vars_no_placeholders() {
        let vars = BTreeMap::new();
        let result = resolve_vars("just a plain string", &vars);
        assert_eq!(result, "just a plain string");
    }

    #[test]
    fn wait_for_default_timeout() {
        let wf: WaitFor = serde_json::from_str(r#"{"pattern": "ready"}"#).unwrap();
        assert_eq!(wf.timeout.as_secs(), 30);
        assert_eq!(wf.pattern.as_str(), "ready");
    }

    #[test]
    fn wait_for_empty_pattern_rejected() {
        let err = serde_json::from_str::<WaitFor>(r#"{"pattern": ""}"#).unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "{err}");
    }

    #[test]
    fn wait_for_nul_pattern_rejected() {
        let err =
            serde_json::from_str::<WaitFor>(r#"{"pattern": "bad\u0000pattern"}"#).unwrap_err();
        assert!(err.to_string().contains("NUL"), "{err}");
    }

    #[test]
    fn wait_for_nonempty_pattern_accepted() {
        let wf: WaitFor = serde_json::from_str(r#"{"pattern": "ready"}"#).unwrap();
        assert_eq!(wf.pattern.as_str(), "ready");
    }

    #[test]
    fn wait_for_zero_timeout_rejected() {
        let err =
            serde_json::from_str::<WaitFor>(r#"{"pattern": "ready", "timeout": 0}"#).unwrap_err();
        assert!(err.to_string().contains("at least 1"), "{err}");
    }

    #[test]
    fn wait_for_nonzero_timeout_accepted() {
        let wf: WaitFor = serde_json::from_str(r#"{"pattern": "ready", "timeout": 5}"#).unwrap();
        assert_eq!(wf.timeout.as_secs(), 5);
    }

    #[test]
    fn resolve_vars_user_defined() {
        let mut vars = BTreeMap::new();
        vars.insert(var_key("project"), var_value("/home/user/myproject"));
        let result = resolve_vars("cd {{project}}", &vars);
        assert_eq!(result, "cd /home/user/myproject");
    }

    #[test]
    fn resolve_vars_builtin_cwd() {
        let vars = BTreeMap::new();
        let result = resolve_vars("cd {{cwd}}", &vars);
        assert_eq!(result, "cd $PWD");
    }

    #[test]
    fn resolve_vars_builtin_date() {
        let vars = BTreeMap::new();
        let result = resolve_vars("echo {{date}}", &vars);
        assert_eq!(result, "echo $(date +%Y-%m-%d)");
    }

    #[test]
    fn resolve_vars_builtin_git_branch() {
        let vars = BTreeMap::new();
        let result = resolve_vars("echo {{git_branch}}", &vars);
        assert_eq!(result, "echo $(git rev-parse --abbrev-ref HEAD)");
    }

    #[test]
    fn resolve_vars_does_not_expand_placeholders_from_user_values() {
        let mut vars = BTreeMap::new();
        vars.insert(var_key("project"), var_value("{{cwd}}"));
        let result = resolve_vars("cd {{project}} && echo {{cwd}}", &vars);
        assert_eq!(result, "cd {{cwd}} && echo $PWD");
    }

    #[test]
    fn resolve_vars_deterministic_with_multiple_vars() {
        let mut vars = BTreeMap::new();
        vars.insert(var_key("b_key"), var_value("B"));
        vars.insert(var_key("a_key"), var_value("A"));
        let result = resolve_vars("{{a_key}}-{{b_key}}", &vars);
        assert_eq!(result, "A-B");
    }

    #[test]
    fn resolve_tmux_arg_vars_builtin_cwd_is_concrete() {
        let vars = BTreeMap::new();
        let cwd = os_string_to_tmux_arg(
            std::env::current_dir().unwrap().into_os_string(),
            "current directory for test",
        )
        .unwrap();
        let result = resolve_tmux_arg_vars("{{cwd}}/project", &vars).unwrap();
        assert_eq!(result.as_str(), format!("{cwd}/project"));
        assert!(!result.as_str().contains("$PWD"));
    }

    #[cfg(unix)]
    #[test]
    fn os_string_to_tmux_arg_rejects_non_utf8() {
        use std::os::unix::ffi::OsStringExt;

        let err = os_string_to_tmux_arg(
            OsString::from_vec(b"/tmp/not-utf8-\xff".to_vec()),
            "current directory for {{cwd}}",
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("non-UTF-8"),
            "error should mention UTF-8: {err:#}"
        );
    }

    #[test]
    fn resolve_tmux_arg_vars_user_var_overrides_builtin() {
        let mut vars = BTreeMap::new();
        vars.insert(var_key("cwd"), var_value("/custom path"));
        let result = resolve_tmux_arg_vars("{{cwd}}/project", &vars).unwrap();
        assert_eq!(result.as_str(), "/custom path/project");
    }

    #[test]
    fn resolve_tmux_arg_vars_rejects_nul_from_vars() {
        let err = TemplateVarValue::new("/tmp\0bad").unwrap_err();
        assert!(err.to_string().contains("NUL"), "unexpected error: {err:?}");
    }

    #[test]
    fn decode_builtin_command_stdout_trims_valid_utf8() {
        let output =
            decode_builtin_command_stdout("git_branch", "git", b"feature/strict-types\n").unwrap();
        assert_eq!(output, "feature/strict-types");
    }

    #[test]
    fn decode_builtin_command_stdout_rejects_non_utf8() {
        let err = decode_builtin_command_stdout("git_branch", "git", b"main\xff\n").unwrap_err();
        assert!(
            err.to_string().contains("non-UTF-8 output"),
            "error should mention UTF-8: {err:#}"
        );
    }

    #[test]
    fn shell_quote_template_vars_builtin_cwd_is_shell_expression() {
        let vars = BTreeMap::new();
        let result = shell_quote_template_vars("{{cwd}}/project", &vars).unwrap();
        assert_eq!(result.as_str(), "\"${PWD}\"'/project'");
    }

    #[test]
    fn shell_quote_template_vars_user_var_overrides_builtin() {
        let mut vars = BTreeMap::new();
        vars.insert(var_key("cwd"), var_value("/custom path"));
        let result = shell_quote_template_vars("{{cwd}}/project", &vars).unwrap();
        assert_eq!(result.as_str(), "'/custom path''/project'");
    }

    #[test]
    fn shell_quote_template_vars_rejects_nul_from_vars() {
        let err = TemplateVarValue::new("/tmp\0bad").unwrap_err();
        assert!(err.to_string().contains("NUL"), "unexpected error: {err:?}");
    }

    #[test]
    fn pane_id_accepts_tmux_pane_id() {
        let id = PaneId::new("%12").unwrap();
        assert_eq!(id.as_str(), "%12");
    }

    #[test]
    fn pane_id_rejects_non_tmux_target() {
        for invalid in ["", "0", "%", "%abc", "$PANE", "%1\0"] {
            assert!(
                PaneId::new(invalid).is_err(),
                "expected invalid pane id: {invalid:?}"
            );
        }
    }

    #[test]
    fn tmux_name_accepts_plain_name() {
        let name = TmuxName::new("main").unwrap();
        assert_eq!(name.as_str(), "main");
    }

    #[test]
    fn tmux_name_rejects_target_separator() {
        let err = TmuxName::new("bad:name").unwrap_err();
        assert!(
            err.to_string().contains("target separator"),
            "unexpected error: {err:?}"
        );
    }

    // ── EnvVar key validation ─────────────────────────────────────────────────

    fn parse_env_key(key: &str) -> Result<EnvVar, serde_json::Error> {
        serde_json::from_str(&format!(r#"{{"key":"{key}","value":"v"}}"#))
    }

    #[test]
    fn env_key_valid_simple() {
        let ev = parse_env_key("FOO_BAR").unwrap();
        assert_eq!(ev.key, "FOO_BAR");
    }

    #[test]
    fn env_key_valid_starts_with_underscore() {
        let ev = parse_env_key("_MY_VAR").unwrap();
        assert_eq!(ev.key, "_MY_VAR");
    }

    #[test]
    fn env_key_valid_lowercase() {
        assert!(parse_env_key("my_var").is_ok());
    }

    #[test]
    fn env_key_invalid_starts_with_digit() {
        let err = parse_env_key("1FOO").unwrap_err();
        assert!(err.to_string().contains("must start with"), "{err}");
    }

    #[test]
    fn env_key_invalid_contains_semicolon() {
        let err = parse_env_key("FOO;rm -rf /").unwrap_err();
        assert!(err.to_string().contains("must contain only"), "{err}");
    }

    #[test]
    fn env_key_invalid_contains_dollar() {
        assert!(parse_env_key("FOO$BAR").is_err());
    }

    #[test]
    fn env_key_invalid_empty() {
        assert!(parse_env_key("").is_err());
    }

    // ── tmux name validation ──────────────────────────────────────────────────

    fn parse_session_name(name: &str) -> Result<Session, serde_json::Error> {
        serde_json::from_str(&format!(
            r#"{{"name":"{name}","windows":[{{"name":"w","layout":{{"type":"pane"}}}}]}}"#
        ))
    }

    fn parse_window_name(name: &str) -> Result<Session, serde_json::Error> {
        serde_json::from_str(&format!(
            r#"{{"name":"s","windows":[{{"name":"{name}","layout":{{"type":"pane"}}}}]}}"#
        ))
    }

    #[test]
    fn session_name_valid() {
        assert!(parse_session_name("my-session").is_ok());
    }

    #[test]
    fn session_name_invalid_colon() {
        let err = parse_session_name("my:session").unwrap_err();
        assert!(err.to_string().contains("':'"), "{err}");
    }

    #[test]
    fn session_name_invalid_empty() {
        assert!(parse_session_name("").is_err());
    }

    #[test]
    fn window_name_valid() {
        assert!(parse_window_name("editor").is_ok());
    }

    #[test]
    fn window_name_invalid_colon() {
        let err = parse_window_name("win:one").unwrap_err();
        assert!(err.to_string().contains("':'"), "{err}");
    }

    #[test]
    fn session_name_invalid_dot() {
        let err = parse_session_name("my.session").unwrap_err();
        assert!(err.to_string().contains("'.'"), "{err}");
    }

    #[test]
    fn window_name_invalid_dot() {
        assert!(parse_window_name("win.sub").is_err());
    }

    #[test]
    fn session_name_allows_hyphen_and_space() {
        assert!(parse_session_name("my-session").is_ok());
        assert!(parse_session_name("my session").is_ok());
    }

    #[test]
    fn session_name_invalid_newline() {
        let err = parse_session_name("my\nsession").unwrap_err();
        assert!(err.to_string().contains("control"), "{err}");
    }

    #[test]
    fn session_name_invalid_tab() {
        assert!(parse_session_name("my\tsession").is_err());
    }

    #[test]
    fn window_name_invalid_newline() {
        assert!(parse_window_name("win\nname").is_err());
    }

    #[test]
    fn env_key_invalid_hyphen() {
        assert!(parse_env_key("FOO-BAR").is_err());
    }

    #[test]
    fn env_key_invalid_dot() {
        assert!(parse_env_key("FOO.BAR").is_err());
    }

    #[test]
    fn env_key_invalid_space() {
        assert!(parse_env_key("FOO BAR").is_err());
    }

    #[test]
    fn env_key_single_underscore_is_valid() {
        assert!(parse_env_key("_").is_ok());
    }

    // ── windows non-empty ─────────────────────────────────────────────────────

    #[test]
    fn windows_empty_is_rejected() {
        let err = serde_json::from_str::<Session>(r#"{"name":"s","windows":[]}"#).unwrap_err();
        assert!(err.to_string().contains("at least one window"), "{err}");
    }

    #[test]
    fn windows_single_is_accepted() {
        let s: Session = serde_json::from_str(
            r#"{"name":"s","windows":[{"name":"w","layout":{"type":"pane"}}]}"#,
        )
        .unwrap();
        assert_eq!(s.windows.len(), 1);
    }

    // ── split ratio bounds ────────────────────────────────────────────────────

    fn parse_ratio(ratio: &str) -> Result<Session, serde_json::Error> {
        serde_json::from_str(&format!(
            r#"{{"name":"s","windows":[{{"name":"w","layout":{{"type":"split","direction":"horizontal","ratio":{ratio},"first":{{"type":"pane"}},"second":{{"type":"pane"}}}}}}]}}"#
        ))
    }

    #[test]
    fn ratio_valid_midrange() {
        assert!(parse_ratio("0.6").is_ok());
        assert!(parse_ratio("0.01").is_ok());
        assert!(parse_ratio("0.98").is_ok());
    }

    #[test]
    fn ratio_near_one_is_rejected() {
        // >= 0.99 rounds the second pane to 0%, which tmux rejects
        assert!(parse_ratio("0.99").is_err());
        assert!(parse_ratio("0.995").is_err());
    }

    #[test]
    fn ratio_zero_is_rejected() {
        let err = parse_ratio("0").unwrap_err();
        assert!(err.to_string().contains("between"), "{err}");
    }

    #[test]
    fn ratio_near_zero_is_rejected() {
        // <= 0.005 rounds the second pane to 100%, which tmux rejects
        assert!(parse_ratio("0.001").is_err());
        assert!(parse_ratio("0.005").is_err());
    }

    #[test]
    fn ratio_lower_bound_accepted() {
        // 0.006 → second pane 99.4% → rounds to 99% — valid
        assert!(parse_ratio("0.006").is_ok());
    }

    #[test]
    fn ratio_one_is_rejected() {
        assert!(parse_ratio("1").is_err());
        assert!(parse_ratio("1.0").is_err());
    }

    #[test]
    fn ratio_above_one_is_rejected() {
        assert!(parse_ratio("1.5").is_err());
    }

    #[test]
    fn ratio_negative_is_rejected() {
        assert!(parse_ratio("-0.3").is_err());
    }

    // ── tmux options key validation ───────────────────────────────────────────

    fn parse_session_options(key: &str) -> Result<Session, serde_json::Error> {
        serde_json::from_str(&format!(
            r#"{{"name":"s","windows":[{{"name":"w","layout":{{"type":"pane"}}}}],"options":{{"{key}":"val"}}}}"#
        ))
    }

    #[test]
    fn options_key_valid() {
        assert!(parse_session_options("status").is_ok());
        assert!(parse_session_options("mouse").is_ok());
    }

    #[test]
    fn options_key_empty_rejected() {
        assert!(parse_session_options("").is_err());
    }

    #[test]
    fn options_key_starting_with_dash_rejected() {
        let err = parse_session_options("-u").unwrap_err();
        assert!(err.to_string().contains("'-'"), "{err}");
    }

    #[test]
    fn options_key_nul_byte_rejected() {
        let err = parse_session_options(r#"bad\u0000key"#).unwrap_err();
        assert!(err.to_string().contains("NUL"), "{err}");
    }

    #[test]
    fn options_value_nul_byte_rejected() {
        let err = serde_json::from_str::<Session>(
            r#"{"name":"s","windows":[{"name":"w","layout":{"type":"pane"}}],"options":{"status":"bad\u0000value"}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("NUL"), "{err}");
    }

    #[test]
    fn window_options_value_nul_byte_rejected() {
        let err = serde_json::from_str::<Session>(
            r#"{"name":"s","windows":[{"name":"w","layout":{"type":"pane"},"options":{"synchronize-panes":"bad\u0000value"}}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("NUL"), "{err}");
    }

    #[test]
    fn select_layout_empty_rejected() {
        let err = serde_json::from_str::<Session>(
            r#"{"name":"s","windows":[{"name":"w","layout":{"type":"pane"},"select_layout":""}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("layout preset"), "{err}");
    }

    #[test]
    fn select_layout_nul_byte_rejected() {
        let err = serde_json::from_str::<Session>(
            r#"{"name":"s","windows":[{"name":"w","layout":{"type":"pane"},"select_layout":"bad\u0000layout"}]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("NUL"), "{err}");
    }

    // ── EnvVar value NUL validation ───────────────────────────────────────────

    #[test]
    fn env_value_nul_byte_rejected() {
        // \u0000 is valid JSON decoded by serde_json to a real NUL byte;
        // std::env::set_var panics on NUL bytes
        let result = serde_json::from_str::<EnvVar>(r#"{"key":"X","value":"\u0000"}"#);
        assert!(result.is_err(), "NUL byte in env value must be rejected");
    }

    #[test]
    fn env_value_normal_accepted() {
        let ev: EnvVar = serde_json::from_str(r#"{"key":"X","value":"hello world"}"#).unwrap();
        assert_eq!(ev.value, "hello world");
    }
}
