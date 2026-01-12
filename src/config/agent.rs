use serde::{Deserialize, Serialize};

/// Configuration for locating custom agent.toml file
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentPathConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_config_path: Option<String>,
    #[serde(
        default,
        alias = "default_model",
        skip_serializing_if = "Option::is_none"
    )]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_enabled: Option<bool>,
}

/// Holds agent path configs from global and workspace
#[derive(Debug, Clone, Default)]
pub struct AgentPathLayers {
    pub global: Option<AgentPathConfig>,
    pub workspace: Option<AgentPathConfig>,
}
