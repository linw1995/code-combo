use serde::{Deserialize, Serialize};

/// Configuration for locating custom agent.toml file
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentPathConfig {
    #[serde(default)]
    pub agent_config_path: Option<String>,
}

/// Holds agent path configs from global and workspace
#[derive(Debug, Clone, Default)]
pub struct AgentPathLayers {
    pub global: Option<AgentPathConfig>,
    pub workspace: Option<AgentPathConfig>,
}
