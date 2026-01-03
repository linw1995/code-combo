use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::config::EnvString;

fn default_socket_path() -> PathBuf {
    PathBuf::from("coco-mcp.sock")
}

fn default_request_timeout_ms() -> u64 {
    10_000
}

fn default_idle_ttl_ms() -> u64 {
    60_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default = "default_socket_path")]
    pub socket_path: PathBuf,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default = "default_idle_ttl_ms")]
    pub idle_ttl_ms: u64,
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

impl McpConfig {
    pub fn resolved_socket_path(&self, config_dir: &Path) -> PathBuf {
        if self.socket_path.is_absolute() {
            self.socket_path.clone()
        } else {
            config_dir.join(&self.socket_path)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(flatten)]
    pub connection: McpServerConnection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConnection {
    Command(McpServerCommandConfig),
    Http(McpServerHttpConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerCommandConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, EnvString>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerHttpConfig {
    pub url: String,
}
