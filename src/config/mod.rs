mod provider;

pub use provider::{ProviderConfig, ProviderKind};

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Config {
    pub providers: Vec<ProviderConfig>,
}

impl Config {
    pub fn parse_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        if path.ends_with(".toml") {
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Err("Unsupported config file format".into())
        }
    }
}
