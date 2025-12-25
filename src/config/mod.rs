mod env;
mod provider;
mod ui;

use std::path::PathBuf;

pub use env::EnvString;
pub use provider::{ProviderConfig, ProviderKind};
pub use ui::{MarkdownRenderEngine, UI};

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Config {
    pub ui: UI,
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub allow_tools: Option<Vec<String>>,
    #[serde(default)]
    pub deny_tools: Option<Vec<String>>,

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

pub fn default_config_dir() -> PathBuf {
    PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("HOME environment variable not set");
        format!("{}/.config", home)
    }))
    .join("coco")
}

#[cfg(test)]
mod tests {
    use super::Config;

    fn base_config() -> String {
        [
            "[ui]",
            "markdown_render_engine = { type = \"native\" }",
            "",
            "[[providers]]",
            "name = \"default\"",
            "kind = \"anthropic\"",
            "api_key = \"test-key\"",
            "base_url = \"https://example.com\"",
            "",
        ]
        .join("\n")
    }

    #[test]
    fn parse_config_without_allow_tools_uses_default() {
        let config: Config = toml::from_str(&base_config()).expect("parse config");
        assert!(config.allow_tools.is_none());
        assert!(config.deny_tools.is_none());
    }

    #[test]
    fn parse_config_with_empty_allow_tools_disables_all() {
        let config_str = format!("allow_tools = []\n{}", base_config());
        let config: Config = toml::from_str(&config_str).expect("parse config");
        assert_eq!(config.allow_tools, Some(Vec::new()));
    }

    #[test]
    fn parse_config_with_empty_deny_tools_is_some() {
        let config_str = format!("deny_tools = []\n{}", base_config());
        let config: Config = toml::from_str(&config_str).expect("parse config");
        assert_eq!(config.deny_tools, Some(Vec::new()));
    }
}
