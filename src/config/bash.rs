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
