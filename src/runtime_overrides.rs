use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use snafu::prelude::*;

use crate::{AgentPathConfig, Config, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeOverrides {
    #[serde(default)]
    pub model_override: Option<String>,
    #[serde(default)]
    pub thinking_enabled: Option<bool>,
    #[serde(default)]
    pub auto_accept_edits: Option<bool>,
}

pub fn runtime_overrides_path(config_dir: &Path) -> PathBuf {
    config_dir.join("config.overwrite.toml")
}

pub fn load_runtime_overrides(config_dir: &Path) -> Result<RuntimeOverrides> {
    let path = runtime_overrides_path(config_dir);
    if !path.exists() {
        return Ok(RuntimeOverrides::default());
    }
    let content =
        fs::read_to_string(&path).whatever_context("failed to read runtime overrides file")?;
    let config: Config =
        toml::from_str(&content).whatever_context("failed to parse runtime overrides file")?;
    let (model_override, thinking_enabled, auto_accept_edits) = match config.agent {
        Some(cfg) => (
            cfg.model.clone(),
            cfg.thinking_enabled,
            cfg.auto_accept_edits,
        ),
        None => (None, None, None),
    };
    let overrides = RuntimeOverrides {
        model_override,
        thinking_enabled,
        auto_accept_edits,
    };
    Ok(overrides)
}

pub fn save_runtime_overrides(config_dir: &Path, overrides: &RuntimeOverrides) -> Result<()> {
    fs::create_dir_all(config_dir).whatever_context("failed to create config dir")?;
    let path = runtime_overrides_path(config_dir);
    let mut config = Config::default();
    if overrides.model_override.is_some()
        || overrides.thinking_enabled.is_some()
        || overrides.auto_accept_edits.is_some()
    {
        config.agent = Some(AgentPathConfig {
            agent_config_path: None,
            model: overrides.model_override.clone(),
            thinking_enabled: overrides.thinking_enabled,
            auto_accept_edits: overrides.auto_accept_edits,
        });
    }
    let output =
        toml::to_string(&config).whatever_context("failed to serialize runtime overrides")?;
    fs::write(&path, output).whatever_context("failed to write runtime overrides file")?;
    Ok(())
}
