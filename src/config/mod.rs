mod provider;
mod ui;

use std::path::PathBuf;

pub use provider::{ProviderConfig, ProviderKind};
pub use ui::{MarkdownRenderEngine, UI};

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Config {
    pub ui: UI,
    pub providers: Vec<ProviderConfig>,

    #[serde(skip)]
    pub config_dir: PathBuf,
}

impl Config {
    pub fn parse_file(path: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let content = std::fs::read_to_string(path)?;
        if path.ends_with(".toml") {
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Err("Unsupported config file format".into())
        }
    }

    pub fn combo_dir(&self) -> PathBuf {
        self.config_dir.join("combos")
    }
}
