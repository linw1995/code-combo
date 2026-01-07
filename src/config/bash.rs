use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SafeCommandsMode {
    #[default]
    Append,
    Override,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BashConfig {
    #[serde(default)]
    pub safe_commands_path: Option<String>,
    #[serde(default)]
    pub safe_commands_mode: SafeCommandsMode,
}

#[derive(Debug, Clone, Default)]
pub struct BashConfigLayers {
    pub global: Option<BashConfig>,
    pub workspace: Option<BashConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SafeCommandEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub allow_any: bool,
    #[serde(default)]
    pub allowed_flags: Vec<String>,
    #[serde(default)]
    pub allow_positional: bool,
    #[serde(default)]
    pub allow_dash: bool,
    #[serde(default)]
    pub flags: Vec<SafeFlagConfig>,
    #[serde(default)]
    pub positional_path_from: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafeFlagConfig {
    pub name: String,
    #[serde(default)]
    pub arg: FlagValuePolicy,
    #[serde(default)]
    pub value: FlagValueType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FlagValuePolicy {
    #[default]
    None,
    Optional,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FlagValueType {
    #[default]
    Any,
    Path,
}
